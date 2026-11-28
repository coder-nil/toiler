//! 子进程的窗口策略：Windows 发布版没有控制台。直接启动 Git、npm 等控制台程序
//! 会新建控制台窗口；仅重定向 stdio 并不能阻止闪窗。

use std::ffi::OsStr;
use std::process::Command;

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
#[cfg(windows)]
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// 构造一个不创建 Windows 控制台窗口的命令，其他平台行为不变。
pub fn command(program: impl AsRef<OsStr>) -> Command {
    command_with_process_group(program, false)
}

/// 为 cloudflared 等需要整组清理的长期进程保留独立进程组，同时避免控制台窗口。
pub fn grouped_command(program: impl AsRef<OsStr>) -> Command {
    command_with_process_group(program, true)
}

/// 不继承启动应用的控制终端。仅重定向 stdin 或另建进程组都挡不住 `/dev/tty`。
pub(crate) fn detached_command(program: impl AsRef<OsStr>) -> Command {
    // Windows 沿用 CREATE_NO_WINDOW；Unix 需要另开会话，而不只是另开进程组。
    let command = command(program);
    #[cfg(unix)]
    let command = {
        use std::os::unix::process::CommandExt;
        let mut command = command;
        // SAFETY: fork 与 exec 之间只调用 setsid；失败时按 errno 返回，不继续启动。
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(())
            });
        }
        command
    };
    command
}

fn command_with_process_group(program: impl AsRef<OsStr>, grouped: bool) -> Command {
    let mut command = Command::new(program);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let group_flag = if grouped { CREATE_NEW_PROCESS_GROUP } else { 0 };
        // `creation_flags` 会替换旧值，必须在同一次调用里传入两个标志。
        command.creation_flags(CREATE_NO_WINDOW | group_flag);
    }
    #[cfg(unix)]
    if grouped {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    command
}

/// Tokio 从同步命令接管整个 `Command`，包括 Windows 的创建标志。
pub fn tokio_command(program: impl AsRef<OsStr>) -> tokio::process::Command {
    tokio::process::Command::from(command(program))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silent_command_still_runs() {
        let output = if cfg!(windows) {
            command("cmd.exe").args(["/D", "/C", "echo", "ok"]).output()
        } else {
            command("echo").arg("ok").output()
        }
        .expect("命令应能启动");
        assert!(output.status.success());
        assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "ok");
    }

    #[cfg(unix)]
    #[test]
    fn detached_command_does_not_inherit_the_terminal_session() {
        const PROBE: &str = "TOILER_TEST_DETACHED_COMMAND_PROBE";
        if std::env::var_os(PROBE).is_some() {
            // CI 本身可能没有终端；检查会话 ID，防止单靠 /dev/tty 的断言误通过。
            assert_eq!(
                unsafe { libc::getsid(0) },
                std::process::id() as libc::pid_t
            );
            assert!(std::fs::File::open("/dev/tty").is_err());
            return;
        }
        let output = detached_command(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "process::tests::detached_command_does_not_inherit_the_terminal_session",
            ])
            .env(PROBE, "1")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "子进程仍在原控制终端的会话：{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[cfg(windows)]
    #[test]
    fn windows_hidden_commands_do_not_open_console() {
        use std::os::windows::process::CommandExt;
        use std::process::Output;
        use windows_sys::Win32::System::{Console::GetConsoleWindow, Threading::DETACHED_PROCESS};
        use windows_sys::Win32::UI::WindowsAndMessaging::IsWindowVisible;

        const STAGE: &str = "TOILER_TEST_CONSOLE_STAGE";
        const ARGS: [&str; 3] = [
            "--exact",
            "process::tests::windows_hidden_commands_do_not_open_console",
            "--nocapture",
        ];
        const PROBED: &str = "TOILER_CONSOLE_PROBED";
        const LAUNCHED: &str = "TOILER_CONSOLE_LAUNCHED";

        fn assert_output(output: Output, marker: &str) {
            let stdout = String::from_utf8_lossy(&output.stdout);
            assert!(
                output.status.success() && stdout.contains(marker),
                "子进程没有通过控制台检查：{}\n{}\n{}",
                output.status,
                stdout,
                String::from_utf8_lossy(&output.stderr),
            );
        }

        // 从没有控制台的进程启动探针，模拟 GUI 发布版。直接由 cargo test 启动
        // 会继承父控制台，且全局枚举窗口会漏掉很短的闪窗或误判其他进程。
        let exe = std::env::current_exe().expect("应能找到当前测试程序");
        match std::env::var(STAGE).as_deref() {
            Ok("probe") => {
                let window = unsafe { GetConsoleWindow() };
                assert!(
                    window.is_null() || unsafe { IsWindowVisible(window) } == 0,
                    "后台子进程不应有可见控制台窗口"
                );
                println!("{PROBED}");
            }
            Ok("launcher") => {
                assert!(unsafe { GetConsoleWindow() }.is_null());
                for mut child in [command(&exe), grouped_command(&exe), detached_command(&exe)] {
                    assert_output(
                        child.args(ARGS).env(STAGE, "probe").output().unwrap(),
                        PROBED,
                    );
                }
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let output = runtime.block_on(async {
                    tokio_command(&exe)
                        .args(ARGS)
                        .env(STAGE, "probe")
                        .output()
                        .await
                        .unwrap()
                });
                assert_output(output, PROBED);
                println!("{LAUNCHED}");
            }
            _ => assert_output(
                Command::new(&exe)
                    .creation_flags(DETACHED_PROCESS)
                    .args(ARGS)
                    .env(STAGE, "launcher")
                    .output()
                    .unwrap(),
                LAUNCHED,
            ),
        }
    }
}
