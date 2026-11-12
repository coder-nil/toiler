//! `/api/mcp` —— MCP 配置文件的读写与探活。
//!
//! 契约事实来源是 pi 的 `app/api/mcp/route.ts` 与 `app/api/mcp/test/route.ts`
//! （从编译产物逐条对齐）。设置面板的 MCP 页一进来就 GET 这条 route，缺了它
//! 会落到静态兜底拿到 404 `{"error":"Not found"}`，页面上直接显示成红字。
//!
//! | 方法 | 行为 |
//! |---|---|
//! | `GET` | 读配置文件 → `{path, servers}`；`includeContent=1` 再带 `content` 原文 |
//! | `PUT` | 校验后原子写回（tmp + rename）→ `{path, servers}` |
//! | `POST` | 把内置 preset 的 `entry` 加进 `mcpServers` → `{path, serverName, added, servers}` |
//! | `POST /api/mcp/test` | 探活：有 `url` 发一次 `initialize`；有 `command` 跑 `--version` |
//!
//! # 与 pi 的差异
//!
//! | 项 | pi | toiler |
//! |---|---|---|
//! | global 配置文件 | `~/.pi/agent/mcp.json` | `<configs>/mcp.json`（`~/.agents/toiler/configs/mcp.json`），与 `models.json` 同一个目录 |
//! | project 配置文件 | `<cwd>/.mcp.json` | 同 |
//! | 运行 MCP 服务器 | `pi-mcp-adapter` 扩展按配置起进程 / 连 SSE，工具进会话 | **不加载**：只读写配置文件、只做探活 |
//! | 探活的 `clientInfo.name` | `pi-desktop` | `toiler` |
//!
//! 换句话说：读写这两半是真的（「编辑 JSON」「使用」都能落盘），但 rpi 没有 MCP
//! 客户端，服务器不会真的连上、工具也不会出现在会话里。这与插件面板同一口径：
//! 界面不假装能用。
//!
//! # 内置 preset
//!
//! 前端「常用 MCP 服务」列表就是这几个条目（pi 的 `lib/mcp-presets.ts`，编译进
//! 前端 chunk）。这里复制同一份数据：`POST` 按 `presetId` 查表，把 `entry` 原样
//! 写进 `mcpServers`。两边不一致时以本文件为准 —— 前端只用来渲染列表。

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::paths;
use crate::security::{is_json_request, Trusted};
use crate::state::AppState;

/// 配置文件不存在时 `GET` 返回的占位内容（与 pi 逐字一致）。
const EMPTY_CONFIG: &str = "{\n  \"mcpServers\": {}\n}\n";

/// 探活超时（pi 的 `AbortSignal.timeout(1e4)` / `execFile` 的 `timeout:1e4`）。
const PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// pi 的 `lib/mcp-presets.ts`：`id` / `name` 用于文案，`entry` 原样写进配置。
const PRESETS: &str = r#"[
  {"id":"deepwiki","name":"DeepWiki","entry":{"url":"https://mcp.deepwiki.com/mcp","protocolVersion":"auto"}},
  {"id":"context7","name":"Context7","entry":{"url":"https://mcp.context7.com/mcp","protocolVersion":"auto"}},
  {"id":"parallel-search","name":"Parallel Search","entry":{"url":"https://search.parallel.ai/mcp","protocolVersion":"auto","directTools":true}},
  {"id":"notion","name":"Notion","entry":{"url":"https://mcp.notion.com/mcp","auth":"oauth","protocolVersion":"auto"}},
  {"id":"github","name":"GitHub","entry":{"url":"https://api.githubcopilot.com/mcp","auth":"oauth","protocolVersion":"auto"}},
  {"id":"chrome-devtools","name":"Chrome DevTools","entry":{"command":"npx","args":["-y","chrome-devtools-mcp@1.6.0"]}}
]"#;

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// 配置文件的作用域。pi 只认 `global`，其余一律当项目。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Scope {
    Global,
    Project,
}

fn parse_scope(raw: Option<&str>) -> Scope {
    match raw {
        Some("global") => Scope::Global,
        _ => Scope::Project,
    }
}

/// 配置文件路径：global 落在 toiler 的 configs/（见文件头的差异表）。
fn config_path(cwd: &Path, scope: Scope) -> PathBuf {
    match scope {
        Scope::Global => paths::config_file("mcp.json"),
        Scope::Project => cwd.join(".mcp.json"),
    }
}

/// `{project_root}/.mcp.json` —— 探活只看项目那份（与 pi 一致）。
fn project_config_path(cwd: &Path) -> PathBuf {
    cwd.join(".mcp.json")
}

/// cwd 校验，顺序与 pi 一致：非空 → 存在 → 是目录 → 在允许根内。
/// 错误用 `(状态码, 文案)` 返回，避免把整个 `Response` 塞进 `Err`（clippy 的
/// `result_large_err`）。
fn resolve_cwd(state: &AppState, raw: Option<&str>) -> Result<PathBuf, (StatusCode, &'static str)> {
    let raw = raw.unwrap_or_default().trim();
    if raw.is_empty() {
        return Err((StatusCode::BAD_REQUEST, "cwd required"));
    }
    let path = paths::absolute(Path::new(raw));
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err((StatusCode::BAD_REQUEST, "cwd must be a directory")),
        Err(_) => return Err((StatusCode::BAD_REQUEST, "Directory does not exist")),
    }
    // 与 `/api/plugins` 相同的两层判定：词法包含，再 realpath 包含。
    state.sync_allowed_roots();
    if !state.roots().allows(&path) || !state.roots().allows_real(&path) {
        return Err((StatusCode::FORBIDDEN, "Access denied"));
    }
    Ok(path)
}

/// 解析配置根：必须是 JSON 对象（`MCP config root must be a JSON object`）。
fn parse_root(text: &str) -> Result<Map<String, Value>, String> {
    let value: Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err("MCP config root must be a JSON object".into()),
    }
}

/// 取 `mcpServers`：缺省算空表，类型不对报错（与 pi 的 `q()` 同语义）。
fn mcp_servers(root: &Map<String, Value>) -> Result<Map<String, Value>, String> {
    match root.get("mcpServers") {
        None => Ok(Map::new()),
        Some(Value::Object(map)) => Ok(map.clone()),
        Some(_) => Err("MCP config mcpServers must be an object".into()),
    }
}

/// 原子写回：先写同级临时文件再 rename（pi 的 `writeFileSync` + `renameSync`）。
fn write_config(
    path: &Path,
    root: &Map<String, Value>,
    servers: Map<String, Value>,
) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut merged = root.clone();
    merged.insert("mcpServers".into(), Value::Object(servers));
    let body = format!(
        "{}\n",
        serde_json::to_string_pretty(&Value::Object(merged)).unwrap_or_else(|_| "{}".into())
    );
    let tmp = path.with_extension(format!("json.{}", std::process::id()));
    std::fs::write(&tmp, body)?;
    std::fs::rename(&tmp, path)
}

/// 内置 preset 查表（`lib/mcp-presets.ts` 的 `findMcpPreset`）。
fn find_preset(id: &str) -> Option<(String, Value)> {
    let list: Value = serde_json::from_str(PRESETS).ok()?;
    let entry = list
        .as_array()?
        .iter()
        .find(|item| item.get("id").and_then(Value::as_str) == Some(id))?;
    let name = entry
        .get("name")
        .and_then(Value::as_str)
        .unwrap_or(id)
        .to_string();
    Some((name, entry.get("entry").cloned().unwrap_or(Value::Null)))
}

// ============================================================================
// handler
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct GetQuery {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    /// 前端发的是 camelCase 的 `includeContent=1`。
    #[serde(default, rename = "includeContent")]
    include_content: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PutBody {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    content: Option<Value>,
}

#[derive(Debug, Deserialize)]
pub struct AddBody {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default, rename = "presetId")]
    preset_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TestBody {
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default, rename = "presetId")]
    preset_id: Option<String>,
}

/// `GET /api/mcp?cwd=&scope=&includeContent=1`
pub async fn get(State(state): State<AppState>, Query(query): Query<GetQuery>) -> Response {
    let scope = parse_scope(query.scope.as_deref());
    let cwd = match resolve_cwd(&state, query.cwd.as_deref()) {
        Ok(path) => path,
        Err((status, message)) => return error(status, message),
    };
    let include_content = query.include_content.as_deref() == Some("1");
    let path = config_path(&cwd, scope);

    let content = std::fs::read_to_string(&path).unwrap_or_else(|_| EMPTY_CONFIG.to_string());
    match parse_root(&content) {
        Ok(root) => match mcp_servers(&root) {
            Ok(servers) => {
                let names: Vec<&String> = servers.keys().collect();
                let mut body = json!({ "path": posix(&path), "servers": names });
                if include_content {
                    body["content"] = json!(content);
                }
                Json(body).into_response()
            }
            Err(message) => error(StatusCode::CONFLICT, &message),
        },
        // 文件本身坏了：带 content 时仍然把原文交回去，前端才能显示坏在哪。
        Err(message) => {
            if include_content {
                Json(json!({
                    "path": posix(&path),
                    "content": content,
                    "servers": [],
                    "parseError": message,
                }))
                .into_response()
            } else {
                error(StatusCode::CONFLICT, &message)
            }
        }
    }
}

/// `PUT /api/mcp` —— 校验后写回整份配置。
pub async fn put(
    State(state): State<AppState>,
    _trusted: Trusted,
    headers: HeaderMap,
    Json(body): Json<PutBody>,
) -> Response {
    if !is_json_request(&headers) {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        );
    }
    let scope = parse_scope(body.scope.as_deref());
    let cwd = match resolve_cwd(&state, body.cwd.as_deref()) {
        Ok(path) => path,
        Err((status, message)) => return error(status, message),
    };
    let Some(Value::String(content)) = body.content else {
        return error(StatusCode::BAD_REQUEST, "content required");
    };
    if scope == Scope::Project {
        let trust = crate::trust::status(state.project_trust(), &cwd.to_string_lossy());
        if trust.requires_trust && !trust.trusted {
            return error(
                StatusCode::FORBIDDEN,
                "Trust this project before editing MCP configuration",
            );
        }
    }
    let root = match parse_root(&content) {
        Ok(root) => root,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let servers = match mcp_servers(&root) {
        Ok(servers) => servers,
        Err(message) => return error(StatusCode::BAD_REQUEST, &message),
    };
    let path = config_path(&cwd, scope);
    if let Err(err) = write_config(&path, &root, servers.clone()) {
        return error(StatusCode::CONFLICT, &err.to_string());
    }
    Json(json!({
        "path": posix(&path),
        "servers": servers.keys().collect::<Vec<_>>(),
    }))
    .into_response()
}

/// `POST /api/mcp` —— 把 preset 的 entry 加进配置文件。
pub async fn add(
    State(state): State<AppState>,
    _trusted: Trusted,
    headers: HeaderMap,
    Json(body): Json<AddBody>,
) -> Response {
    if !is_json_request(&headers) {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        );
    }
    let scope = parse_scope(body.scope.as_deref());
    let cwd = match resolve_cwd(&state, body.cwd.as_deref()) {
        Ok(path) => path,
        Err((status, message)) => return error(status, message),
    };
    let Some(preset_id) = body.preset_id.as_deref().map(str::trim) else {
        return error(StatusCode::BAD_REQUEST, "presetId required");
    };
    if preset_id.is_empty() {
        return error(StatusCode::BAD_REQUEST, "presetId required");
    }
    let Some((_name, entry)) = find_preset(preset_id) else {
        return error(StatusCode::NOT_FOUND, "Unknown MCP preset");
    };
    if scope == Scope::Project {
        let trust = crate::trust::status(state.project_trust(), &cwd.to_string_lossy());
        if trust.requires_trust && !trust.trusted {
            return error(
                StatusCode::FORBIDDEN,
                "Trust this project before adding MCP servers",
            );
        }
    }

    let path = config_path(&cwd, scope);
    let content = std::fs::read_to_string(&path).unwrap_or_else(|_| EMPTY_CONFIG.to_string());
    let root = match parse_root(&content) {
        Ok(root) => root,
        Err(message) => return error(StatusCode::CONFLICT, &message),
    };
    let mut servers = match mcp_servers(&root) {
        Ok(servers) => servers,
        Err(message) => return error(StatusCode::CONFLICT, &message),
    };
    let already = servers.contains_key(preset_id);
    servers.entry(preset_id.to_string()).or_insert(entry);
    if !already {
        if let Err(err) = write_config(&path, &root, servers.clone()) {
            return error(StatusCode::CONFLICT, &err.to_string());
        }
    }
    Json(json!({
        "path": posix(&path),
        "serverName": preset_id,
        "added": !already,
        "servers": servers.keys().collect::<Vec<_>>(),
    }))
    .into_response()
}

/// `POST /api/mcp/test` —— 探活（不发真实 MCP 会话，只确认端点/命令存在）。
pub async fn test(
    State(state): State<AppState>,
    _trusted: Trusted,
    headers: HeaderMap,
    Json(body): Json<TestBody>,
) -> Response {
    if !is_json_request(&headers) {
        return error(
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        );
    }
    let cwd = match resolve_cwd(&state, body.cwd.as_deref()) {
        Ok(path) => path,
        Err((status, message)) => return error(status, message),
    };
    let Some(preset_id) = body.preset_id.as_deref().map(str::trim) else {
        return error(StatusCode::BAD_REQUEST, "presetId required");
    };
    if preset_id.is_empty() {
        return error(StatusCode::BAD_REQUEST, "presetId required");
    }
    let Some((name, _)) = find_preset(preset_id) else {
        return error(StatusCode::NOT_FOUND, "Unknown MCP preset");
    };
    let trust = crate::trust::status(state.project_trust(), &cwd.to_string_lossy());
    if trust.requires_trust && !trust.trusted {
        return error(
            StatusCode::FORBIDDEN,
            "Trust this project before testing MCP servers",
        );
    }

    // pi 只从项目那份 `.mcp.json` 里取条目，不看 global。
    let path = project_config_path(&cwd);
    let entry = std::fs::read_to_string(&path)
        .ok()
        .and_then(|text| parse_root(&text).ok())
        .and_then(|root| mcp_servers(&root).ok())
        .and_then(|servers| servers.get(preset_id).cloned())
        .filter(Value::is_object);

    let Some(entry) = entry else {
        return error(
            StatusCode::CONFLICT,
            &format!("{name} is not configured in .mcp.json"),
        );
    };
    let entry = entry.as_object().cloned().unwrap_or_default();

    if let Some(url) = entry
        .get("url")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Json(probe_url(&name, url).await).into_response();
    }
    if let Some(command) = entry
        .get("command")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        return Json(probe_command(&name, command).await).into_response();
    }
    Json(json!({
        "status": "failed",
        "message": format!("{name} has neither a URL nor a command configured."),
    }))
    .into_response()
}

/// 对 `url` 发一次 `initialize`：能拿到 event-stream 或 JSON-RPC 回包就算通了。
async fn probe_url(name: &str, url: &str) -> Value {
    let payload = json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "toiler", "version": env!("CARGO_PKG_VERSION") },
        },
    });
    let client = match reqwest::Client::builder().timeout(PROBE_TIMEOUT).build() {
        Ok(client) => client,
        Err(err) => {
            return json!({ "status": "failed", "message": format!("{name} could not be reached: {err}") })
        }
    };
    let response = client
        .post(url)
        .header("Accept", "application/json, text/event-stream")
        .json(&payload)
        .send()
        .await;

    let response = match response {
        Ok(response) => response,
        Err(err) => {
            return json!({
                "status": "failed",
                "message": format!("{name} could not be reached: {err}"),
            })
        }
    };
    let status = response.status();
    let code = status.as_u16();
    if code == 401 || code == 403 {
        return json!({
            "status": "auth-required",
            "message": format!("{name} is reachable but requires authorization."),
            "httpStatus": code,
        });
    }
    if !status.is_success() {
        return json!({
            "status": "failed",
            "message": format!("{name} responded with HTTP {code}."),
            "httpStatus": code,
        });
    }
    let is_stream = response
        .headers()
        .get("content-type")
        .and_then(|value| value.to_str().ok())
        .map(|value| value.to_ascii_lowercase().contains("event-stream"))
        .unwrap_or(false);
    if is_stream {
        return json!({
            "status": "passed",
            "message": format!("{name} responded as an MCP endpoint."),
            "httpStatus": code,
        });
    }
    let body = response.text().await.unwrap_or_default();
    if body_has_jsonrpc(&body) {
        return json!({
            "status": "passed",
            "message": format!("{name} responded as an MCP endpoint."),
            "httpStatus": code,
        });
    }
    json!({
        "status": "unknown",
        "message": format!("{name} responded, but the MCP protocol could not be confirmed."),
        "httpStatus": code,
    })
}

/// 与 pi 的 `/\"jsonrpc\"\s*:\s*\"2\.0\"/` 同义，不用正则（免得引入额外依赖）。
fn body_has_jsonrpc(body: &str) -> bool {
    let mut rest = body;
    while let Some(index) = rest.find("\"jsonrpc\"") {
        let after = &rest[index + "\"jsonrpc\"".len()..];
        let trimmed = after.trim_start();
        if let Some(after_colon) = trimmed.strip_prefix(':') {
            if after_colon.trim_start().starts_with("\"2.0\"") {
                return true;
            }
        }
        rest = after;
    }
    false
}

/// `command --version` 能跑起来就算「已配置」（进程在真正用到工具时才由 MCP
/// 插件拉起 —— 本仓库不加载插件，这层如实说明）。
async fn probe_command(name: &str, command: &str) -> Value {
    let mut cmd = tokio::process::Command::new(command);
    cmd.arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    match tokio::time::timeout(PROBE_TIMEOUT, cmd.status()).await {
        Ok(Ok(status)) if status.success() => json!({
            "status": "configured",
            "message": format!(
                "{name} is configured. Its local process will start when an MCP tool is used."
            ),
        }),
        Ok(Ok(status)) => json!({
            "status": "failed",
            "message": format!(
                "{name} command \"{command}\" exited with {}.",
                status.code().map(|c| c.to_string()).unwrap_or_else(|| "a signal".into())
            ),
        }),
        Ok(Err(err)) => json!({
            "status": "failed",
            "message": format!("{name} command \"{command}\" is not available: {err}"),
        }),
        Err(_) => json!({
            "status": "failed",
            "message": format!("{name} command \"{command}\" timed out."),
        }),
    }
}

/// 统一 POSIX 分隔符（Windows 上 `join` 会给出 `\`）。
fn posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toiler-mcp-{tag}-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn project_config_lives_next_to_the_project() {
        let cwd = Path::new("/tmp/project");
        assert_eq!(
            config_path(cwd, Scope::Project),
            PathBuf::from("/tmp/project/.mcp.json")
        );
        // global 落在 toiler 自己的 configs/ 下，而不是 pi 的 agent 目录。
        let global = config_path(cwd, Scope::Global);
        assert_eq!(global, paths::config_file("mcp.json"));
        assert!(global.ends_with("configs/mcp.json"));
    }

    #[test]
    fn missing_file_reads_as_empty_server_map() {
        let root = parse_root(EMPTY_CONFIG).unwrap();
        assert!(mcp_servers(&root).unwrap().is_empty());
    }

    #[test]
    fn servers_must_be_an_object() {
        let root = parse_root(r#"{"mcpServers": [1,2]}"#).unwrap();
        assert_eq!(
            mcp_servers(&root).unwrap_err(),
            "MCP config mcpServers must be an object"
        );
    }

    #[test]
    fn root_must_be_an_object() {
        assert_eq!(
            parse_root("[1]").unwrap_err(),
            "MCP config root must be a JSON object"
        );
    }

    #[test]
    fn write_is_atomic_and_keeps_other_keys() {
        let dir = temp_dir("write");
        let path = dir.join(".mcp.json");
        let root = parse_root(r#"{"theme":"x"}"#).unwrap();
        let mut servers = Map::new();
        servers.insert("deepwiki".into(), json!({"url": "https://example.com"}));
        write_config(&path, &root, servers).unwrap();

        let text = std::fs::read_to_string(&path).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["theme"], json!("x"));
        assert_eq!(
            parsed["mcpServers"]["deepwiki"]["url"],
            json!("https://example.com")
        );
        // 临时文件不留在目录里。
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name() != ".mcp.json")
            .collect();
        assert!(leftovers.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn presets_are_known_by_id() {
        let (name, entry) = find_preset("deepwiki").unwrap();
        assert_eq!(name, "DeepWiki");
        assert_eq!(entry["url"], json!("https://mcp.deepwiki.com/mcp"));
        assert!(find_preset("nope").is_none());
    }

    #[test]
    fn jsonrpc_probe_ignores_whitespace() {
        assert!(body_has_jsonrpc(r#"{"jsonrpc" : "2.0"}"#));
        assert!(body_has_jsonrpc("  \"jsonrpc\":\n \"2.0\""));
        assert!(!body_has_jsonrpc(r#"{"jsonrpc":"1.0"}"#));
        assert!(!body_has_jsonrpc("plain text"));
    }
}
