//! 桌面专属的版本升级桥：只执行随应用内嵌的官方脚本，不执行项目提供的 Python。
//! 不放进 `/api`，避免手机与隧道访问获得执行项目脚本或创建发布提交的能力。

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Stdio;

use serde::Deserialize;
use serde_json::{json, Value};
use toiler_server::gitsvc;

const SCRIPT: &str = include_str!("../../../tools/bump-version.py");
const DRIVER: &str = r#"
import contextlib, io, json, sys
request = json.loads(sys.argv[2])
scope = {"__name__": "toiler_bump_version", "__file__": sys.argv[1]}
output = io.StringIO()
try:
    with contextlib.redirect_stdout(output):
        exec(compile(sys.stdin.read(), sys.argv[1], "exec"), scope)
        result = scope["desktop_release"](
            request["level"], dry_run=request["dryRun"],
            expected_head=request.get("expectedHead"),
            expected_version=request.get("expectedVersion"),
            expected_plan=request.get("expectedPlan"),
        )
    result["output"] = output.getvalue()
    print(json.dumps(result, ensure_ascii=False))
except Exception as error:
    print(json.dumps({"error": str(error), "output": output.getvalue()}, ensure_ascii=False))
    sys.exit(1)
"#;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpgradeRequest {
    cwd: String,
    level: String,
    dry_run: bool,
    expected_head: Option<String>,
    expected_version: Option<String>,
    expected_plan: Option<String>,
}

/// 与普通 Git 写操作共用仓库锁，整段升级/提交/标签流程不能和面板其它动作交错。
fn upgrade(request: UpgradeRequest) -> Result<Value, String> {
    if !matches!(request.level.as_str(), "patch" | "minor" | "major") {
        return Err("未知的升级级别".into());
    }
    if !Path::new(&request.cwd).is_absolute() {
        return Err("项目路径必须是绝对路径".into());
    }
    let root = gitsvc::repository_root(Path::new(&request.cwd)).ok_or("不是 Git 仓库")?;
    let lock = gitsvc::repo_lock(&root);
    let _guard = lock.lock().map_err(|_| "仓库正在执行其它操作")?;
    let script_path = root.join("tools/bump-version.py");
    let script_root = script_path
        .canonicalize()
        .ok()
        .and_then(|path| path.parent().and_then(Path::parent).map(Path::to_path_buf));
    if script_root.is_none() || script_root != root.canonicalize().ok() {
        return Err(
            "项目缺少 tools/bump-version.py 或脚本路径包含符号链接；只支持与该脚本兼容的项目"
                .into(),
        );
    }
    let (python, prefix) = find_python()?;
    let mut command = crate::hidden_command(&python);
    command
        .args(&prefix)
        .args(["-I", "-X", "utf8", "-c", DRIVER]);
    command.arg(&script_path).arg(
        json!({
            "level": request.level,
            "dryRun": request.dry_run,
            "expectedHead": request.expected_head,
            "expectedVersion": request.expected_version,
            "expectedPlan": request.expected_plan,
        })
        .to_string(),
    );
    command
        .current_dir(&root)
        .env("PYTHONIOENCODING", "utf-8")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command
        .spawn()
        .map_err(|error| format!("无法启动版本升级：{error}"))?;
    let write_result = child
        .stdin
        .take()
        .ok_or("无法传入内置脚本")?
        .write_all(SCRIPT.as_bytes());
    if let Err(error) = write_result {
        let _ = child.kill();
        let _ = child.wait();
        return Err(format!("无法传入内置脚本：{error}"));
    }
    let output = child
        .wait_with_output()
        .map_err(|error| error.to_string())?;
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| {
        format!(
            "版本升级返回异常：{}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    if let Some(error) = value.get("error").and_then(Value::as_str) {
        return Err(error.into());
    }
    if !output.status.success() {
        return Err("版本升级失败，请检查项目状态".into());
    }
    Ok(value)
}

fn find_python() -> Result<(PathBuf, Vec<&'static str>), String> {
    for (name, prefix) in [("python3", vec![]), ("python", vec![]), ("py", vec!["-3"])] {
        let output = crate::hidden_command(name)
            .args(&prefix)
            .args([
                "-I",
                "-c",
                "import sys; sys.exit(0 if sys.version_info >= (3, 9) else 1)",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if output.is_ok_and(|status| status.success()) {
            return Ok((PathBuf::from(name), prefix));
        }
    }
    Err("版本升级需要 Python 3.9 或更高版本，请安装并加入 PATH 后重试".into())
}

#[tauri::command]
pub async fn upgrade_project_version(request: UpgradeRequest) -> Result<Value, String> {
    tauri::async_runtime::spawn_blocking(move || upgrade(request))
        .await
        .map_err(|error| error.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn request_rejects_unknown_options_and_missing_confirmation_mode() {
        assert!(serde_json::from_value::<UpgradeRequest>(json!({
            "cwd": "/tmp/project", "level": "patch", "dryRun": true, "command": "other"
        }))
        .is_err());
        assert!(serde_json::from_value::<UpgradeRequest>(json!({
            "cwd": "/tmp/project", "level": "patch"
        }))
        .is_err());
    }

    #[test]
    fn invalid_levels_and_relative_paths_never_spawn_processes() {
        let request = |cwd: &str, level: &str| UpgradeRequest {
            cwd: cwd.into(),
            level: level.into(),
            dry_run: true,
            expected_head: None,
            expected_version: None,
            expected_plan: None,
        };
        assert!(upgrade(request("/tmp/project", "command"))
            .unwrap_err()
            .contains("级别"));
        assert!(upgrade(request("relative", "patch"))
            .unwrap_err()
            .contains("绝对路径"));
    }

    #[test]
    fn embedded_script_uses_same_prerelease_rules() {
        // 这里只加载官方脚本验证规则，不升级真实仓库；完整流程由隔离仓库测试覆盖。
        let (python, prefix) = find_python().expect("测试环境需要 Python 3.9+");
        let mut command = crate::hidden_command(&python);
        command.args(prefix).args(["-I", "-c", "import sys; scope={'__name__':'test','__file__':sys.argv[1]}; exec(sys.stdin.read(),scope); assert scope['next_version']('1.2.3-beta.1','patch')[0]=='1.2.3'"])
            .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/bump-version.py"))
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(SCRIPT.as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(DRIVER.contains("desktop_release"));
    }
}
