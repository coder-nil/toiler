//! 内置终端：给前端 `TerminalPanel` 提供真实的 PTY。
//!
//! 阶段 4 只留了四个「明确报错」的命令占位（错误文案是「阶段 5 再评估」），
//! 前端 `TerminalPanel.tsx` 与 `capabilities/default.json` 一直是完整的 ——
//! 也就是说这块只差后端。现在补上，并与 pi 的行为逐项对齐：
//!
//! | | pi-desktop | toiler |
//! |---|---|---|
//! | PTY | `portable-pty` 0.9 | 同一版本、同一套调用 |
//! | shell | unix 用 `$SHELL`（`new_default_prog`），Windows 用 `powershell.exe` | 一致 |
//! | 输出通道 | `terminal-output` 事件（`Vec<u8>`） | 一致（前端 `term.write(new Uint8Array(...))`） |
//! | 退出通道 | `terminal-exit` 事件 | 一致 |
//! | 并发上限 | 16 个 | 一致 |
//! | locale | 继承值不是 UTF-8 时补 `LC_*` | 一致（否则中文输入法回显成 `\M-ã…`） |
//!
//! 为什么这块必须留在桌面壳里、不进 `toiler-server`：它开的是**任意 shell**，
//! 而服务端还要对局域网和隧道暴露 `/api`。把起进程的能力放在只有窗口能调到的
//! Tauri 命令里，手机链路就天然没有这条路径。

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use portable_pty::{native_pty_system, Child as PtyChild, CommandBuilder, MasterPty, PtySize};
use serde::Serialize;
use tauri::{Emitter, Manager};

/// 同时开着的终端上限。每个 PTY 是一个真实 shell，不限会在误操作时堆进程。
const MAX_TERMINALS: usize = 16;

/// 单次键入的字节上限（粘贴一大段也算一次）。超过必然是异常输入。
const MAX_INPUT_BYTES: usize = 1024 * 1024;

/// 事件发往的窗口 label（`create_main_window` 建的就是 `main`）。
const MAIN_WINDOW: &str = "main";

/// 一个活着的终端：master 用来 resize，writer 用来键入，child 用来回收。
struct TerminalSession {
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    child: Box<dyn PtyChild + Send + Sync>,
}

impl Drop for TerminalSession {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            // 交互式作业有自己的前台进程组（比如在 shell 里跑了 vim）。
            // 关面板时先杀它，再杀 shell 自己，否则 `npm run dev` 这类子进程会留下。
            if let Some(group) = self.master.process_group_leader() {
                if group > 0 {
                    unsafe {
                        libc::kill(-group, libc::SIGKILL);
                    }
                }
            }
            if let Some(pid) = self.child.process_id() {
                if pid > 0 {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
            }
        }
        #[cfg(windows)]
        if let Some(pid) = self.child.process_id() {
            let _ = crate::hidden_command("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .status();
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// 终端注册表（Tauri managed state）。
pub struct TerminalState {
    sessions: Mutex<HashMap<String, TerminalSession>>,
    next_id: AtomicU64,
}

impl Default for TerminalState {
    fn default() -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        }
    }
}

impl TerminalState {
    /// 当前活着的终端数（退出时打印一行日志，测试也用它）。
    pub fn live_count(&self) -> usize {
        self.sessions.lock().map(|s| s.len()).unwrap_or(0)
    }
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalOutput {
    terminal_id: String,
    data: Vec<u8>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct TerminalExit {
    terminal_id: String,
}

/// 建一对 PTY + 起 shell。返回会话句柄和输出读取端（读取端交给后台线程）。
fn create_terminal(
    command: CommandBuilder,
    cols: u16,
    rows: u16,
) -> Result<(TerminalSession, Box<dyn Read + Send>), String> {
    let pair = native_pty_system()
        .openpty(pty_size(cols, rows))
        .map_err(|error| format!("无法创建终端：{error}"))?;
    let reader = pair
        .master
        .try_clone_reader()
        .map_err(|error| format!("无法读取终端输出：{error}"))?;
    let writer = pair
        .master
        .take_writer()
        .map_err(|error| format!("无法写入终端：{error}"))?;
    let child = pair
        .slave
        .spawn_command(command)
        .map_err(|error| format!("无法启动 shell：{error}"))?;
    // slave 必须尽早关掉：留着它，shell 退出后 master 仍能读到一个未关闭的
    // 从端，`terminal-exit` 就永远不会触发（前端看不到「Shell 已退出」）。
    drop(pair.slave);
    Ok((
        TerminalSession {
            master: pair.master,
            writer,
            child,
        },
        reader,
    ))
}

/// 把前端尺寸夹到合理区间：xterm 在高度极小的瞬间会送 `rows: 1`，
/// 直接透传给内核会让 shell 的提示符错乱。
fn pty_size(cols: u16, rows: u16) -> PtySize {
    PtySize {
        rows: rows.clamp(2, 500),
        cols: cols.clamp(2, 500),
        pixel_width: 0,
        pixel_height: 0,
    }
}

fn is_utf8_locale(value: &str) -> bool {
    let lower = value.trim().to_ascii_lowercase();
    lower.contains("utf-8") || lower.contains("utf8")
}

fn utf8_locale_value() -> &'static str {
    if cfg!(target_os = "macos") {
        "en_US.UTF-8"
    } else {
        "C.UTF-8"
    }
}

/// 真正起作用的 locale 变量：`LC_ALL` 覆盖 `LC_CTYPE`，后者覆盖 `LANG`。
fn effective_locale() -> Option<(String, String)> {
    ["LC_ALL", "LC_CTYPE", "LANG"].iter().find_map(|key| {
        let value = std::env::var(key).ok()?;
        (!value.trim().is_empty()).then(|| (key.to_string(), value))
    })
}

/// 需要写进 PTY 环境的覆盖项；空列表表示继承值已是 UTF-8，保持原样
/// （用户显式的 `zh_CN.UTF-8` 不该被改写）。
fn utf8_locale_overrides(inherited: Option<(String, String)>) -> Vec<(String, &'static str)> {
    let value = utf8_locale_value();
    match inherited {
        Some((key, current)) if is_utf8_locale(&current) => {
            let _ = key;
            Vec::new()
        }
        // 继承到的是非 UTF-8（GUI 启动常常是 C/POSIX）：换掉真正起作用的那一项，
        // 并补 `LC_CTYPE`，让宽字符与输入法在 shell 与各子进程之间保持一致。
        Some((key, _)) => {
            let needs_ctype = key != "LC_CTYPE";
            let mut overrides = vec![(key, value)];
            if needs_ctype {
                overrides.push(("LC_CTYPE".to_string(), value));
            }
            overrides
        }
        // 完全没有 locale（从访达/程序坞拉起时就是这样）。
        None => vec![("LANG".to_string(), value), ("LC_CTYPE".to_string(), value)],
    }
}

fn apply_locale_overrides(command: &mut CommandBuilder, inherited: Option<(String, String)>) {
    for (key, value) in utf8_locale_overrides(inherited) {
        command.env(key, value);
    }
}

/// 组装 shell 命令：cwd + `TERM` + UTF-8 locale。
fn shell_command(directory: &std::path::Path) -> CommandBuilder {
    let mut command = if cfg!(windows) {
        CommandBuilder::new("powershell.exe")
    } else {
        CommandBuilder::new_default_prog()
    };
    command.cwd(directory);
    command.env("TERM", "xterm-256color");
    command.env("COLORTERM", "truecolor");
    #[cfg(unix)]
    apply_locale_overrides(&mut command, effective_locale());
    command
}

/// 校验 cwd：必须是绝对路径且真的是目录。
///
/// 与 pi 的差别只有一条：多拒一个相对路径。桌面壳从访达启动时 cwd 是 `/`，
/// 相对路径会解析到应用包内部（`Toiler.app/Contents/MacOS/…`），
/// 用户看到的终端落在谁也想不到的地方。
fn resolve_cwd(cwd: &str) -> Result<PathBuf, String> {
    let trimmed = cwd.trim();
    if trimmed.is_empty() {
        return Err("终端工作目录为空。".to_string());
    }
    let directory = PathBuf::from(trimmed);
    if !directory.is_absolute() {
        return Err(format!("终端工作目录必须是绝对路径：{trimmed}"));
    }
    if !directory.is_dir() {
        return Err(format!("终端工作目录不存在：{trimmed}"));
    }
    Ok(directory)
}

#[tauri::command]
pub fn terminal_start(
    app: tauri::AppHandle,
    state: tauri::State<'_, TerminalState>,
    cwd: String,
    cols: u16,
    rows: u16,
) -> Result<String, String> {
    let directory = resolve_cwd(&cwd)?;
    let command = shell_command(&directory);

    let mut sessions = state
        .sessions
        .lock()
        .map_err(|_| "终端注册表不可用。".to_string())?;
    if sessions.len() >= MAX_TERMINALS {
        return Err(format!(
            "最多同时打开 {MAX_TERMINALS} 个终端，请先关闭一个。"
        ));
    }
    let (session, mut reader) = create_terminal(command, cols, rows)?;
    let terminal_id = format!("terminal-{}", state.next_id.fetch_add(1, Ordering::Relaxed));
    sessions.insert(terminal_id.clone(), session);
    drop(sessions);

    let output_id = terminal_id.clone();
    std::thread::spawn(move || {
        let mut buffer = [0_u8; 8192];
        loop {
            match reader.read(&mut buffer) {
                // shell 退出（或被关掉）时读端先返回 0；读错误同样没有后续输出。
                Ok(0) | Err(_) => break,
                Ok(length) => {
                    let _ = app.emit_to(
                        MAIN_WINDOW,
                        "terminal-output",
                        TerminalOutput {
                            terminal_id: output_id.clone(),
                            data: buffer[..length].to_vec(),
                        },
                    );
                }
            }
        }
        // 读循环结束 = shell 已经不在了，先摘掉注册表里的条目（否则它就泄漏了）。
        let state = app.state::<TerminalState>();
        let removed = state
            .sessions
            .lock()
            .ok()
            .and_then(|mut sessions| sessions.remove(&output_id));
        drop(removed);
        let _ = app.emit_to(
            MAIN_WINDOW,
            "terminal-exit",
            TerminalExit {
                terminal_id: output_id,
            },
        );
    });

    Ok(terminal_id)
}

#[tauri::command]
pub fn terminal_write(
    state: tauri::State<'_, TerminalState>,
    terminal_id: String,
    data: Vec<u8>,
) -> Result<(), String> {
    if data.len() > MAX_INPUT_BYTES {
        return Err("终端输入过大。".to_string());
    }
    let mut sessions = state
        .sessions
        .lock()
        .map_err(|_| "终端注册表不可用。".to_string())?;
    let session = sessions
        .get_mut(&terminal_id)
        .ok_or_else(|| "终端未在运行。".to_string())?;
    session
        .writer
        .write_all(&data)
        .map_err(|error| format!("写入终端失败：{error}"))?;
    session
        .writer
        .flush()
        .map_err(|error| format!("刷新终端输入失败：{error}"))
}

#[tauri::command]
pub fn terminal_resize(
    state: tauri::State<'_, TerminalState>,
    terminal_id: String,
    cols: u16,
    rows: u16,
) -> Result<(), String> {
    let sessions = state
        .sessions
        .lock()
        .map_err(|_| "终端注册表不可用。".to_string())?;
    let session = sessions
        .get(&terminal_id)
        .ok_or_else(|| "终端未在运行。".to_string())?;
    session
        .master
        .resize(pty_size(cols, rows))
        .map_err(|error| format!("调整终端尺寸失败：{error}"))
}

#[tauri::command]
pub fn terminal_close(
    state: tauri::State<'_, TerminalState>,
    terminal_id: String,
) -> Result<(), String> {
    // 关闭必须幂等：前端在「shell 自己退出」之后也会再调一次，那时条目已经被
    // 读循环摘掉了。返回错误会让面板显示一条假告警。
    let session = state
        .sessions
        .lock()
        .map_err(|_| "终端注册表不可用。".to_string())?
        .remove(&terminal_id);
    drop(session);
    Ok(())
}

/// 退出应用前清空注册表：每个 `TerminalSession` 的 `Drop` 会整组杀掉 shell。
pub fn stop_terminals(app: &tauri::AppHandle) {
    if let Some(state) = app.try_state::<TerminalState>() {
        let live = state.live_count();
        if live > 0 {
            tracing::info!("回收 {live} 个终端");
        }
        if let Ok(mut sessions) = state.sessions.lock() {
            sessions.clear();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn rejects_relative_and_missing_directories() {
        assert!(resolve_cwd("").is_err());
        assert!(resolve_cwd("relative/path").is_err());
        assert!(resolve_cwd("/definitely/not/here/toiler").is_err());
        let temp = std::env::temp_dir();
        assert_eq!(resolve_cwd(&temp.to_string_lossy()).unwrap(), temp);
    }

    #[test]
    fn pty_size_is_clamped() {
        let tiny = pty_size(0, 1);
        assert_eq!((tiny.cols, tiny.rows), (2, 2));
        let huge = pty_size(9000, 9000);
        assert_eq!((huge.cols, huge.rows), (500, 500));
        let normal = pty_size(120, 40);
        assert_eq!((normal.cols, normal.rows), (120, 40));
    }

    #[test]
    fn utf8_locale_detection_accepts_common_spellings() {
        assert!(is_utf8_locale("en_US.UTF-8"));
        assert!(is_utf8_locale("zh_CN.utf8"));
        assert!(is_utf8_locale(" C.UTF-8 "));
        assert!(!is_utf8_locale("C"));
        assert!(!is_utf8_locale("POSIX"));
        assert!(!is_utf8_locale(""));
    }

    #[test]
    fn utf8_locale_overrides_replace_a_c_locale_and_keep_a_utf8_one() {
        let value = utf8_locale_value();
        // 已经是 UTF-8 就不动：用户显式的 zh_CN.UTF-8 不该被改成 en_US。
        assert!(
            utf8_locale_overrides(Some(("LANG".to_string(), "zh_CN.UTF-8".to_string()))).is_empty()
        );
        // LC_ALL 覆盖其它变量，所以必须连它一起换掉，否则覆盖不生效。
        assert_eq!(
            utf8_locale_overrides(Some(("LC_ALL".to_string(), "C".to_string()))),
            vec![
                ("LC_ALL".to_string(), value),
                ("LC_CTYPE".to_string(), value)
            ]
        );
        assert_eq!(
            utf8_locale_overrides(Some(("LC_CTYPE".to_string(), "POSIX".to_string()))),
            vec![("LC_CTYPE".to_string(), value)]
        );
        assert_eq!(
            utf8_locale_overrides(None),
            vec![("LANG".to_string(), value), ("LC_CTYPE".to_string(), value)]
        );
    }

    #[test]
    fn terminal_state_starts_empty_and_ids_increment() {
        let state = TerminalState::default();
        assert_eq!(state.live_count(), 0);
        assert_eq!(state.next_id.fetch_add(1, Ordering::Relaxed), 1);
        assert_eq!(state.next_id.fetch_add(1, Ordering::Relaxed), 2);
    }

    /// 端到端：真的起一个 shell，键入、读回、resize，并确认 drop 会杀掉它。
    ///
    /// 这条用例守的是「PTY 接错了」这类只在真实进程里才暴露的问题（比如忘了
    /// `drop(slave)` 就读不到 EOF、resize 没传到内核）。
    #[cfg(unix)]
    #[test]
    fn shell_echoes_input_reports_cwd_and_tracks_resize() {
        let directory = std::env::temp_dir().canonicalize().unwrap();
        let mut command = CommandBuilder::new("/bin/sh");
        command.args([
            "-c",
            "read value; printf 'received:%s\\n' \"$value\"; pwd; stty size",
        ]);
        command.cwd(&directory);
        let (mut session, mut reader) = create_terminal(command, 80, 24).unwrap();
        session
            .master
            .resize(pty_size(100, 30))
            .expect("resize 应当成功");
        assert_eq!(session.master.get_size().unwrap().cols, 100);

        let (sender, receiver) = mpsc::channel();
        std::thread::spawn(move || {
            let mut output = Vec::new();
            let _ = reader.read_to_end(&mut output);
            let _ = sender.send(output);
        });
        session.writer.write_all(b"hello-terminal\n").unwrap();
        session.writer.flush().unwrap();
        let output = receiver.recv_timeout(Duration::from_secs(5)).unwrap();
        let text = String::from_utf8_lossy(&output);
        assert!(text.contains("received:hello-terminal"), "{text}");
        assert!(text.contains(directory.to_str().unwrap()), "{text}");
        assert!(text.contains("30 100"), "resize 没传到内核：{text}");
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_session_kills_the_shell() {
        let mut command = CommandBuilder::new("/bin/sh");
        command.args(["-c", "exec sleep 30"]);
        let (session, _reader) = create_terminal(command, 80, 24).unwrap();
        let pid = session.child.process_id().unwrap() as i32;
        drop(session);
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1, "shell 应当已被杀掉");
    }
}
