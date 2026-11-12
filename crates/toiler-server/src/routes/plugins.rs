//! 插件面板：`GET /api/plugins` 只读列出「配置了哪些包、装没装、包里有什么」。
//!
//! pi 的 `app/api/plugins/route.ts` 是围绕 `DefaultPackageManager` 的一整套控制台：
//! 跑 npm/git 安装、解析 `package.json` 的 `pi` 清单、把包里发现的扩展/技能/提示词/
//! 主题并入资源池，并支持 install / remove / update / enable / disable。toiler
//! 只做「看」这一半——但必须做，否则前端拉 `GET /api/plugins` 会落到静态兜底拿到
//! 404 `{"error":"Not found"}`，插件页左侧就会把这个 error 当成一条插件画出来。
//!
//! # 为什么装上了也不等于能用
//!
//! `packages` 里几乎都是 npm 上的 pi 扩展包，主体是 TypeScript 扩展；rpi 是 Rust
//! 运行时（cdylib ABI），**不会**加载它们。所以面板的 install/remove 只是把包
//! 装到磁盘、把条目写进 `settings.json`，并如实显示 Loaded/版本/资源——**运行时行为
//! 不变**。这一点必须在界面上说清，否则「装成功但没效果」比装不上更让人困惑。
//!
//! # 安装根为什么是 `plugins/` 而不是 agent 目录
//!
//! pi 把包装在 `<agentDir>/npm`、`<agentDir>/git`；toiler 的 agent 目录是
//! `~/.agents/toiler`，那样放包会让「插件」与「配置 / 会话 / 项目库」挤在同一层，
//! 也和 `paths.rs` 早就声明的 `plugins/` 目录约定冲突。所以全局包一律落在
//! `<toilerHome>/plugins` 下，内部保留按来源分类的子目录（与 pi 同构，便于对照）：
//!
//! ```text
//! ~/.agents/toiler/plugins/
//! ├── npm/node_modules/<name>/     npm: 来源
//! └── git/<host>/<path>/           git: 来源
//! ```
//!
//! # 与 pi 的差异
//!
//! | 项 | pi | toiler |
//! |---|---|---|
//! | 包来源 | 全局 + 项目 `settings.json` 的 `packages` | 同；项目未信任时忽略项目那份（同 pi） |
//! | 安装目录 | `<agentDir>/npm/node_modules/<name>`，缺省时退回全局 npm root / pnpm | `<toilerHome>/plugins/{npm/node_modules,git}/…`；**不**回退 `~/.pi/agent/npm`，也不跑 `npm root -g` |
//! | 安装 / 卸载 / 更新 / 启停 | 调 npm / git、改 `settings.json` | npm 走**本机 npm**（`--legacy-peer-deps`），启停改 `settings.json`；**git 来源不支持**（需手动 clone）；找不到 npm 时 500 + 中文提示 |
//! | 资源发现 | 递归时按 `.gitignore` / `.ignore` / `.fdignore` 过滤 | 只跳过 `.` 开头与 `node_modules`（不解析忽略文件） |
//! | 清单里的 glob 条目 | `*` `?` 展开 | 跳过该条目 |
//! | `packages` 的对象形态（过滤清单） | 按模式逐文件决定启用 | 只认「数组为空 = 该类型全禁用」，非空数组不做模式匹配（按清单全量统计） |
//! | `totals` | 包资源 + `settings.json` 顶层条目 + 自动发现目录 | 只统计包内资源 |
//! | 插件市场 | `/api/plugins/catalog` 搜 npm、`/catalog/translate` 翻描述 | 空表（PLAN「明确不做的」里的技能/插件市场） |
//!
//! 只产出两条诊断，文案与 pi 逐字一致（`Package is configured but not installed yet.` /
//! `Configured package path was not found.`）：前端把它们做成「N diagnostics」的悬停
//! 提示，改文案只会让两边截图对不上。

use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::paths;
use crate::state::AppState;

/// 一条 `packages` 条目的作用域。pi 内部叫 `user`，对外 JSON 里叫 `global`。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scope {
    Global,
    Project,
}

impl Scope {
    fn label(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

/// 一条 `packages` 条目（字符串，或带过滤清单的对象）。
struct PackageEntry {
    source: String,
    scope: Scope,
    /// 对象形态 = 带 `extensions`/`skills`/… 过滤清单（pi 的 `filtered`）。
    filtered: bool,
    /// 四个数组都为空 = 用户禁用了这个包。
    disabled: bool,
    /// 对象形态的原始值，资源发现时要看它的空数组。
    filter: Option<Value>,
}

/// 资源种类，顺序与 pi 的 `RESOURCE_TYPES` 一致。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Extensions,
    Skills,
    Prompts,
    Themes,
}

impl Kind {
    const ALL: [Kind; 4] = [Kind::Extensions, Kind::Skills, Kind::Prompts, Kind::Themes];

    /// 约定目录名与 `package.json` 里 `pi` 清单的键名（同名）。
    fn dir(self) -> &'static str {
        match self {
            Self::Extensions => "extensions",
            Self::Skills => "skills",
            Self::Prompts => "prompts",
            Self::Themes => "themes",
        }
    }

    /// 前端按这个值分组（`PluginResourceInfo.kind`）。
    fn singular(self) -> &'static str {
        match self {
            Self::Extensions => "extension",
            Self::Skills => "skill",
            Self::Prompts => "prompt",
            Self::Themes => "theme",
        }
    }
}

/// `packages` 条目解析出来的来源类型。
enum Source {
    Npm {
        name: String,
        version: Option<String>,
    },
    Git {
        host: String,
        path: String,
        git_ref: Option<String>,
    },
    Local {
        path: String,
    },
}

#[derive(Debug, Deserialize)]
pub struct PluginsQuery {
    /// 项目目录。pi 缺省时 400，这里保持一致。
    cwd: Option<String>,
}

/// `POST /api/plugins` 的请求体（pi 的 `PluginAction` payload）。
#[derive(Debug, Deserialize)]
pub struct ActionBody {
    cwd: Option<String>,
    action: Option<String>,
    source: Option<String>,
    scope: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CatalogQuery {
    #[serde(default)]
    from: Option<i64>,
    #[serde(default)]
    size: Option<i64>,
}

// ============================================================================
// handler
// ============================================================================

pub async fn list(State(state): State<AppState>, Query(query): Query<PluginsQuery>) -> Response {
    let raw_cwd = query.cwd.unwrap_or_default();
    let cwd = raw_cwd.trim();
    if cwd.is_empty() {
        return error(StatusCode::BAD_REQUEST, "cwd required");
    }

    // 授权与 `/api/skills` 完全相同：先词法包含，再 realpath 包含。
    // `sync_allowed_roots` 不能省：允许根是每次判定前重建的。
    state.sync_allowed_roots();
    let cwd_path = paths::absolute(Path::new(cwd));
    if !state.roots().allows(&cwd_path) || !state.roots().allows_real(&cwd_path) {
        return error(StatusCode::FORBIDDEN, "Access denied");
    }

    let trusted = crate::trust::status(state.project_trust(), cwd).trusted;
    Json(snapshot(&state, &cwd_path, trusted)).into_response()
}

/// `POST /api/plugins`：install / remove / update / disable / enable。
///
/// 装包走**本机 npm**（`npm install <spec> --prefix <安装根> --legacy-peer-deps`），
/// 然后改写 `settings.json` 的 `packages`；与 pi 的 `DefaultPackageManager` 同语义。
/// 找不到 npm 时返回 500 + 中文提示（不静默失败：面板会直接把这条画出来）。
///
/// 为什么装完还是没有真正的功能：包里的扩展是 TypeScript，rpi 不会加载，
/// 面板会显示 Loaded，运行时行为不变。
pub async fn command(State(state): State<AppState>, Json(body): Json<ActionBody>) -> Response {
    let raw_cwd = body.cwd.clone().unwrap_or_default();
    let cwd = raw_cwd.trim().to_string();
    if cwd.is_empty() {
        return error(StatusCode::BAD_REQUEST, "cwd required");
    }

    state.sync_allowed_roots();
    let cwd_path = paths::absolute(Path::new(&cwd));
    if !state.roots().allows(&cwd_path) || !state.roots().allows_real(&cwd_path) {
        return error(StatusCode::FORBIDDEN, "Access denied");
    }

    let action = body.action.as_deref().unwrap_or("").trim().to_string();
    if action.is_empty() {
        return error(StatusCode::BAD_REQUEST, "action required");
    }
    let scope = match body.scope.as_deref() {
        Some("project") => Scope::Project,
        _ => Scope::Global,
    };
    let trusted = crate::trust::status(state.project_trust(), &cwd).trusted;
    // 项目插件只有在项目被信任时才能改：否则一个刚 clone 下来的仓库就能靠
    // `.pi/settings.json` 决定要装什么。文案与 pi 一致，前端直接展示。
    if scope == Scope::Project && !trusted {
        return error(
            StatusCode::FORBIDDEN,
            "Project resources must be trusted before modifying project plugins",
        );
    }

    let source = body.source.as_deref().unwrap_or("").trim().to_string();
    if matches!(action.as_str(), "install" | "remove" | "disable" | "enable") && source.is_empty() {
        return error(StatusCode::BAD_REQUEST, "source required");
    }
    // 面板上的按钮点了没反应时，先看这条日志：没打印 = 请求根本没到服务端
    // （前端报 `Failed to fetch` 基本就是这种：服务已退出 / 页面指向了别的实例）。
    tracing::info!(action = %action, source = %source, scope = scope.label(), "插件操作");

    let root = plugins_root(&state);
    let settings_file = settings_path(&state, scope, &cwd_path);
    let outcome = match action.as_str() {
        "install" => install(&source, scope, &cwd_path, &root, &settings_file).await,
        "remove" => remove(&source, scope, &cwd_path, &root, &settings_file).await,
        "update" => update(&source, scope, &cwd_path, &root).await,
        "disable" => set_disabled(&settings_file, &source, scope, &cwd_path, &root, true),
        "enable" => set_disabled(&settings_file, &source, scope, &cwd_path, &root, false),
        other => {
            return error(
                StatusCode::BAD_REQUEST,
                &format!("Unsupported action: {other}"),
            )
        }
    };

    match outcome {
        Ok(()) => Json(snapshot(&state, &cwd_path, trusted)).into_response(),
        Err(failure) => error(failure.status, &failure.message),
    }
}

/// `GET /api/plugins/catalog`：pi 在这里搜索 npm 上的 pi 扩展包。
///
/// 市场明确不做（PLAN「明确不做的」），但也不能 404：前端打开「添加插件」抽屉时
/// 会把响应当错误画出来。返回空页 = 抽屉显示「没有结果」。
pub async fn catalog(Query(query): Query<CatalogQuery>) -> Response {
    Json(json!({
        "plugins": [],
        "from": query.from.unwrap_or(0),
        "size": query.size.unwrap_or(40),
        "hasMore": false,
    }))
    .into_response()
}

/// `POST /api/plugins/catalog/translate`：pi 把英文描述翻成中文。
///
/// 没有目录可翻，返回空映射即可（前端拿不到条目就跳过翻译）。
pub async fn catalog_translate() -> Response {
    Json(json!({ "translations": {} })).into_response()
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

// ============================================================================
// 写侧：安装 / 卸载 / 更新 / 启停
// ============================================================================

/// 写侧失败：状态码 + 给前端直接展示的一条消息。
struct Failure {
    status: StatusCode,
    message: String,
}

impl Failure {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self::new(StatusCode::INTERNAL_SERVER_ERROR, message)
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::internal(format!("写文件失败：{error}"))
    }
}

/// 该作用域的 `settings.json`：全局 = toiler 的配置目录，项目 = `<cwd>/.pi`。
fn settings_path(state: &AppState, scope: Scope, cwd: &Path) -> PathBuf {
    match scope {
        Scope::Global => state.settings().path().to_path_buf(),
        Scope::Project => cwd.join(".pi").join("settings.json"),
    }
}

/// 该作用域的 npm 安装根（pi 的 `getNpmInstallRoot`）：
/// 全局 `<pluginsRoot>/npm`，项目 `<cwd>/.pi/npm`。
fn npm_root(scope: Scope, cwd: &Path, plugins_root: &Path) -> PathBuf {
    match scope {
        Scope::Global => plugins_root.join("npm"),
        Scope::Project => cwd.join(".pi").join("npm"),
    }
}

/// 装包时 `npm` 需要一个工程目录：建目录 + `.gitignore` + 最小 `package.json`
/// （pi 的 `ensureNpmProject`，形状保持一致，便于两边对照）。
fn ensure_npm_project(dir: &Path) -> Result<(), Failure> {
    std::fs::create_dir_all(dir)?;
    let ignore_path = dir.join(".gitignore");
    if !ignore_path.exists() {
        std::fs::write(&ignore_path, "*\n!.gitignore\n")?;
    }
    let manifest_path = dir.join("package.json");
    if !manifest_path.exists() {
        let body = serde_json::to_string_pretty(&json!({
            "name": "pi-extensions",
            "private": true,
        }))
        .unwrap_or_else(|_| "{}".into());
        std::fs::write(&manifest_path, format!("{body}\n"))?;
    }
    Ok(())
}

/// 找到 npm 可执行文件。
///
/// 从 Finder / launchd 启动时 `PATH` 往往只有 `/usr/bin:/bin:/usr/sbin:/sbin`，
/// 而 npm 通常装在 nvm / Homebrew 目录里，所以 `PATH` 找不到时再探一遍常见位置。
/// `TOILER_NPM` 可以显式指定（测试与非常规安装用）。
fn npm_executable() -> Result<PathBuf, Failure> {
    if let Some(raw) = std::env::var_os("TOILER_NPM") {
        let path = PathBuf::from(raw);
        return if path.is_file() {
            Ok(path)
        } else {
            Err(missing_npm(&format!(
                "TOILER_NPM 指向的文件不存在：{}",
                path.display()
            )))
        };
    }

    if let Some(path) = find_in_path("npm") {
        return Ok(path);
    }

    let mut candidates: Vec<PathBuf> = vec![
        PathBuf::from("/opt/homebrew/bin/npm"),
        PathBuf::from("/usr/local/bin/npm"),
        PathBuf::from("/usr/bin/npm"),
    ];
    // nvm：取版本号最大的一套（按目录名排序即可，`v24` > `v22`）。
    if let Some(home) = std::env::var_os("HOME") {
        let nvm_root = PathBuf::from(home).join(".nvm/versions/node");
        if let Ok(entries) = std::fs::read_dir(&nvm_root) {
            let mut versions: Vec<PathBuf> = entries
                .flatten()
                .map(|entry| entry.path().join("bin/npm"))
                .filter(|path| path.is_file())
                .collect();
            versions.sort();
            candidates.splice(0..0, versions.into_iter().rev());
        }
    }

    candidates
        .into_iter()
        .find(|path| path.is_file())
        .ok_or_else(|| {
            missing_npm("未检测到 npm。请先安装 Node.js（自带 npm），或确认 npm 在 PATH 中")
        })
}

fn find_in_path(name: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.is_file())
}

fn missing_npm(detail: &str) -> Failure {
    Failure::new(
        StatusCode::INTERNAL_SERVER_ERROR,
        format!("{detail}；插件安装依赖本机 npm"),
    )
}

/// `npm` 一次安装/卸载最长等待时间。装个 pi 扩展通常几秒，卡住说明网络或 registry 有问题。
const NPM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(300);

/// 跑一次 npm，非零退出时把 stderr 尾部原样带回（`ETARGET` 这类信息对用户最有用）。
async fn run_npm(npm: &Path, args: &[String]) -> Result<(), Failure> {
    let mut command = tokio::process::Command::new(npm);
    command
        .args(args)
        .stdin(std::process::Stdio::null())
        .kill_on_drop(true);

    let label = args.first().cloned().unwrap_or_default();
    tracing::info!(npm = %npm.display(), args = %args.join(" "), "执行 npm");
    let output = match tokio::time::timeout(NPM_TIMEOUT, command.output()).await {
        Ok(Ok(output)) => output,
        Ok(Err(error)) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(missing_npm("未检测到 npm"));
        }
        Ok(Err(error)) => return Err(Failure::internal(format!("执行 npm 失败：{error}"))),
        Err(_) => {
            return Err(Failure::internal(format!(
                "npm {label} 超时（{} 秒）：检查网络或 npm registry",
                NPM_TIMEOUT.as_secs()
            )))
        }
    };

    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    let tail: String = stderr
        .chars()
        .rev()
        .take(600)
        .collect::<Vec<char>>()
        .into_iter()
        .rev()
        .collect();
    let code = output
        .status
        .code()
        .map(|code| code.to_string())
        .unwrap_or_else(|| "未知".into());
    tracing::warn!(code = %code, stderr = %tail.trim(), "npm 失败");
    Err(Failure::internal(format!(
        "npm {label} 失败（退出码 {code}）：{}",
        tail.trim()
    )))
}

/// `npm:` 前缀后面的 spec（`pi-mcp-adapter@7.0.1`），install 直接交给 npm。
fn npm_spec(raw: &str) -> String {
    raw.trim()
        .strip_prefix("npm:")
        .unwrap_or(raw.trim())
        .trim()
        .to_string()
}

async fn install(
    source: &str,
    scope: Scope,
    cwd: &Path,
    plugins_root: &Path,
    settings_file: &Path,
) -> Result<(), Failure> {
    match parse_source(source) {
        Source::Npm { .. } => {
            let npm = npm_executable()?;
            let root = npm_root(scope, cwd, plugins_root);
            ensure_npm_project(&root)?;
            let spec = npm_spec(source);
            run_npm(
                &npm,
                &[
                    "install".into(),
                    spec,
                    "--prefix".into(),
                    posix(&root),
                    "--legacy-peer-deps".into(),
                ],
            )
            .await?;
        }
        Source::Local { path } => {
            let resolved = local_path(&path, scope, cwd, plugins_root);
            if !resolved.exists() {
                return Err(Failure::new(
                    StatusCode::BAD_REQUEST,
                    format!("Path does not exist: {}", resolved.display()),
                ));
            }
        }
        Source::Git { .. } => {
            return Err(Failure::new(
                StatusCode::BAD_REQUEST,
                "暂不支持 git: 来源的插件：请先手动 git clone 到 <toilerHome>/plugins/git/<host>/<path>，再把条目写进 settings.json",
            ));
        }
    }
    add_source_to_settings(settings_file, source, scope, cwd, plugins_root)
}

async fn remove(
    source: &str,
    scope: Scope,
    cwd: &Path,
    plugins_root: &Path,
    settings_file: &Path,
) -> Result<(), Failure> {
    if let Source::Npm { name, .. } = parse_source(source) {
        let root = npm_root(scope, cwd, plugins_root);
        // 目录不存在 = 从来没装过，跳过 npm，只清 settings（pi 的行为）。
        if root.is_dir() {
            let npm = npm_executable()?;
            run_npm(
                &npm,
                &[
                    "uninstall".into(),
                    name,
                    "--prefix".into(),
                    posix(&root),
                    "--legacy-peer-deps".into(),
                ],
            )
            .await?;
        }
    } else if matches!(parse_source(source), Source::Git { .. }) {
        return Err(Failure::new(
            StatusCode::BAD_REQUEST,
            "暂不支持 git: 来源的插件卸载：请手动删除 plugins/git 下的目录",
        ));
    }
    remove_source_from_settings(settings_file, source, scope, cwd, plugins_root)
}

async fn update(
    source: &str,
    scope: Scope,
    cwd: &Path,
    plugins_root: &Path,
) -> Result<(), Failure> {
    match parse_source(source) {
        Source::Npm {
            version: Some(version),
            ..
        } if is_exact_version(&version) => {
            // 精确版本 = 用户钉死的，pi 也不动它。
            return Ok(());
        }
        Source::Npm { name, version } => {
            let npm = npm_executable()?;
            let root = npm_root(scope, cwd, plugins_root);
            ensure_npm_project(&root)?;
            let spec = if version.is_some() {
                // 有范围（`^1.2.0`）就按配置的 spec 重装，npm 会解析到范围内的最新。
                npm_spec(source)
            } else {
                format!("{name}@latest")
            };
            run_npm(
                &npm,
                &[
                    "install".into(),
                    spec,
                    "--prefix".into(),
                    posix(&root),
                    "--legacy-peer-deps".into(),
                ],
            )
            .await?;
        }
        Source::Local { .. } => {}
        Source::Git { .. } => {
            return Err(Failure::new(
                StatusCode::BAD_REQUEST,
                "暂不支持 git: 来源的插件更新：请在 plugins/git/<host>/<path> 里手动 git pull",
            ));
        }
    }
    Ok(())
}

/// 启用 / 禁用（pi 的 `setPackageDisabled`）：禁用 = 条目变成「四个数组都为空」的对象，
/// 启用 = 条目变回字符串。只改 `settings.json`，不碰磁盘上的包。
fn set_disabled(
    settings_file: &Path,
    source: &str,
    scope: Scope,
    cwd: &Path,
    plugins_root: &Path,
    disabled: bool,
) -> Result<(), Failure> {
    let mut root = read_json(settings_file);
    let Some(object) = root.as_object_mut() else {
        return Err(Failure::internal(format!(
            "{} 不是 JSON 对象",
            settings_file.display()
        )));
    };
    let list = object
        .entry("packages")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(list) = list.as_array_mut() else {
        return Err(Failure::internal("settings.json 的 packages 不是数组"));
    };

    let identity = source_identity(source, scope, cwd, plugins_root);
    let Some(index) = find_entry(list, &identity, scope, cwd, plugins_root) else {
        return Err(Failure::new(
            StatusCode::BAD_REQUEST,
            format!("settings.json 里没有这个包：{source}"),
        ));
    };

    let entry = &list[index];
    let normalized = if disabled {
        let mut object = match entry {
            Value::Object(existing) => existing.clone(),
            _ => {
                let mut map = Map::new();
                map.insert("source".into(), json!(source));
                map
            }
        };
        for kind in Kind::ALL {
            object.insert(kind.dir().into(), Value::Array(Vec::new()));
        }
        Value::Object(object)
    } else {
        json!(source)
    };
    list[index] = normalized;

    write_settings(settings_file, &root)
}

/// 把条目加进 `settings.json` 的 `packages`（同身份的条目做替换而不是重复追加）。
fn add_source_to_settings(
    settings_file: &Path,
    source: &str,
    scope: Scope,
    cwd: &Path,
    plugins_root: &Path,
) -> Result<(), Failure> {
    let mut root = read_json(settings_file);
    let Some(object) = root.as_object_mut() else {
        return Err(Failure::internal(format!(
            "{} 不是 JSON 对象",
            settings_file.display()
        )));
    };
    let list = object
        .entry("packages")
        .or_insert_with(|| Value::Array(Vec::new()));
    let Some(list) = list.as_array_mut() else {
        return Err(Failure::internal("settings.json 的 packages 不是数组"));
    };

    let identity = source_identity(source, scope, cwd, plugins_root);
    match find_entry(list, &identity, scope, cwd, plugins_root) {
        Some(index) => match &mut list[index] {
            Value::Object(existing) => {
                existing.insert("source".into(), json!(source));
            }
            entry => *entry = json!(source),
        },
        None => list.push(json!(source)),
    }
    write_settings(settings_file, &root)
}

fn remove_source_from_settings(
    settings_file: &Path,
    source: &str,
    scope: Scope,
    cwd: &Path,
    plugins_root: &Path,
) -> Result<(), Failure> {
    let mut root = read_json(settings_file);
    let Some(object) = root.as_object_mut() else {
        return Err(Failure::internal(format!(
            "{} 不是 JSON 对象",
            settings_file.display()
        )));
    };
    if let Some(list) = object.get_mut("packages").and_then(Value::as_array_mut) {
        let identity = source_identity(source, scope, cwd, plugins_root);
        list.retain(|entry| {
            entry_identity(entry, scope, cwd, plugins_root) != Some(identity.clone())
        });
    }
    write_settings(settings_file, &root)
}

fn find_entry(
    list: &[Value],
    identity: &str,
    scope: Scope,
    cwd: &Path,
    plugins_root: &Path,
) -> Option<usize> {
    list.iter().position(|entry| {
        entry_identity(entry, scope, cwd, plugins_root).as_deref() == Some(identity)
    })
}

fn entry_identity(entry: &Value, scope: Scope, cwd: &Path, plugins_root: &Path) -> Option<String> {
    let source = match entry {
        Value::String(source) => source.as_str(),
        Value::Object(_) => entry.get("source").and_then(Value::as_str)?,
        _ => return None,
    };
    Some(source_identity(source, scope, cwd, plugins_root))
}

/// 身份键（pi 的 `getSourceMatchKeyForSettings`）：npm 只认包名、git 认 host/path、
/// 本地路径按作用域 base 解析成绝对路径。用它判断「settings 里是否已经有这个包」。
fn source_identity(source: &str, scope: Scope, cwd: &Path, plugins_root: &Path) -> String {
    match parse_source(source) {
        Source::Npm { name, .. } => format!("npm:{name}"),
        Source::Git { host, path, .. } => format!("git:{host}/{path}"),
        Source::Local { path } => format!(
            "local:{}",
            posix(&local_path(&path, scope, cwd, plugins_root))
        ),
    }
}

fn local_path(raw: &str, scope: Scope, cwd: &Path, plugins_root: &Path) -> PathBuf {
    let expanded = paths::expand_tilde(raw.trim());
    if expanded.is_absolute() {
        return paths::absolute(&expanded);
    }
    let base = match scope {
        Scope::Global => plugins_root.to_path_buf(),
        Scope::Project => cwd.join(".pi"),
    };
    paths::absolute(&base.join(expanded))
}

/// 精确版本（`1.2.3`）= 用户钉死的版本，`update` 不动它（与 pi 的 `isExactNpmVersion` 同义）。
fn is_exact_version(version: &str) -> bool {
    let core = version.trim_start_matches('v');
    let mut parts = core.split('.');
    let mut count = 0;
    for part in parts.by_ref() {
        let digits: String = part.chars().take_while(|ch| ch.is_ascii_digit()).collect();
        if digits.is_empty() {
            return false;
        }
        count += 1;
        if count == 3 {
            break;
        }
    }
    count == 3
}

fn write_settings(path: &Path, value: &Value) -> Result<(), Failure> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let body = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".into());
    std::fs::write(path, format!("{body}\n"))?;
    Ok(())
}

// ============================================================================
// 组装响应
// ============================================================================

fn snapshot(state: &AppState, cwd: &Path, trusted: bool) -> Value {
    let root = plugins_root(state);
    let global = entries_from(&state.settings().read(), Scope::Global);
    // 项目未信任时 pi 的 `SettingsManager` 直接返回空对象，项目包不出现。
    let project_settings = if trusted {
        read_json(&cwd.join(".pi").join("settings.json"))
    } else {
        Value::Null
    };
    let project = entries_from(&project_settings, Scope::Project);

    // 解析顺序与 pi 的 `resolve()` 一致：项目在前（同身份时项目优先）。
    let mut resolve_order: Vec<&PackageEntry> = project.iter().collect();
    resolve_order.extend(global.iter());

    let mut diagnostics: Vec<Value> = Vec::new();
    for entry in &resolve_order {
        if installed_path(&entry.source, entry.scope, cwd, &root).is_none() {
            diagnostics.push(json!({
                "type": "warning",
                "source": entry.source,
                "message": "Package is configured but not installed yet.",
            }));
        }
    }

    let mut totals = vec![0usize; 4];
    let mut packages: Vec<Value> = Vec::new();
    // `listConfiguredPackages()` 的顺序：全局在前，项目在后（**不**去重）。
    for entry in global.iter().chain(project.iter()) {
        let installed = installed_path(&entry.source, entry.scope, cwd, &root);
        if installed.is_none() {
            diagnostics.push(json!({
                "type": "warning",
                "source": entry.source,
                "message": "Configured package path was not found.",
            }));
        }

        let resources = installed
            .as_deref()
            .map(|root| collect_resources(root, entry.filter.as_ref()))
            .unwrap_or_default();

        let mut counts = vec![0usize; 4];
        let mut resources_json: Vec<Value> = Vec::new();
        for (kind, path) in &resources {
            let index = kind_index(*kind);
            counts[index] += 1;
            totals[index] += 1;
            resources_json.push(json!({
                "kind": kind.singular(),
                "name": resource_name(path, *kind),
                "path": posix(path),
                "relativePath": installed
                    .as_deref()
                    .and_then(|root| relative_from(root, path))
                    .unwrap_or_else(|| posix(path)),
            }));
        }

        let (package_name, version) = installed
            .as_deref()
            .map(package_metadata)
            .unwrap_or((None, None));
        let parsed = parse_source(&entry.source);
        let configured_version = configured_version(&parsed);

        let resource_count: usize = counts.iter().sum();
        let status = if entry.disabled {
            "disabled"
        } else if resource_count > 0 {
            "loaded"
        } else if installed.is_some() {
            "installed"
        } else {
            "missing"
        };

        let mut object = Map::new();
        object.insert("source".into(), json!(entry.source));
        object.insert("scope".into(), json!(entry.scope.label()));
        object.insert("filtered".into(), json!(entry.filtered));
        object.insert("disabled".into(), json!(entry.disabled));
        // pi 用 `JSON.stringify` 输出，`undefined` 的键会整个消失；下面保持同样行为。
        if let Some(path) = installed.as_deref() {
            object.insert("installedPath".into(), json!(posix(path)));
        }
        if let Some(name) = package_name {
            object.insert("packageName".into(), json!(name));
        }
        if let Some(version) = version {
            object.insert("version".into(), json!(version));
        }
        if let Some(version) = configured_version {
            object.insert("configuredVersion".into(), json!(version));
        }
        object.insert("counts".into(), counts_json(&counts));
        object.insert("resources".into(), Value::Array(resources_json));
        object.insert("status".into(), json!(status));
        packages.push(Value::Object(object));
    }

    json!({
        "packages": packages,
        "totals": counts_json(&totals),
        "diagnostics": diagnostics,
        "projectResourcesLoaded": trusted,
    })
}

fn counts_json(counts: &[usize]) -> Value {
    json!({
        "extensions": counts[0],
        "skills": counts[1],
        "prompts": counts[2],
        "themes": counts[3],
    })
}

fn kind_index(kind: Kind) -> usize {
    match kind {
        Kind::Extensions => 0,
        Kind::Skills => 1,
        Kind::Prompts => 2,
        Kind::Themes => 3,
    }
}

/// toiler 的插件安装根（`<toilerHome>/plugins`）。
///
/// 生产的 `settings.json` 在 `<toilerHome>/configs/settings.json`（见 `paths.rs` 的
/// 目录布局），而测试里 `with_stores` 往往直接指向 `<tmp>/settings.json`。用配置
/// 目录反推工作区根再拼 `plugins`，两种情况都对，也免了在 `AppState` 里再加字段。
fn plugins_root(state: &AppState) -> PathBuf {
    let settings = state.settings().path();
    let home = match settings.parent() {
        Some(configs) if configs.file_name().is_some_and(|name| name == "configs") => configs
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| configs.to_path_buf()),
        Some(dir) => dir.to_path_buf(),
        None => return paths::plugins_dir(),
    };
    home.join("plugins")
}

// ============================================================================
// settings.json → 条目
// ============================================================================

fn read_json(path: &Path) -> Value {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(Value::Null)
}

fn entries_from(settings: &Value, scope: Scope) -> Vec<PackageEntry> {
    let Some(list) = settings.get("packages").and_then(Value::as_array) else {
        return Vec::new();
    };
    list.iter()
        .filter_map(|item| match item {
            Value::String(source) => Some(PackageEntry {
                source: source.clone(),
                scope,
                filtered: false,
                disabled: false,
                filter: None,
            }),
            Value::Object(_) => {
                let source = item.get("source").and_then(Value::as_str)?;
                Some(PackageEntry {
                    source: source.to_string(),
                    scope,
                    filtered: true,
                    disabled: is_disabled_entry(item),
                    filter: Some(item.clone()),
                })
            }
            _ => None,
        })
        .collect()
}

/// 四个资源数组都存在且都为空 = 禁用（pi 的 `isDisabledPackage`）。
fn is_disabled_entry(item: &Value) -> bool {
    Kind::ALL.iter().all(|kind| {
        item.get(kind.dir())
            .and_then(Value::as_array)
            .is_some_and(|list| list.is_empty())
    })
}

// ============================================================================
// 来源解析
// ============================================================================

fn parse_source(raw: &str) -> Source {
    let trimmed = raw.trim();
    if let Some(spec) = trimmed.strip_prefix("npm:") {
        let (name, version) = split_npm_spec(spec.trim());
        return Source::Npm { name, version };
    }
    if let Some(git) = parse_git(trimmed) {
        return git;
    }
    Source::Local {
        path: trimmed.to_string(),
    }
}

/// `@scope/name@range` / `name@range` / `name` → `(name, version)`。
fn split_npm_spec(spec: &str) -> (String, Option<String>) {
    // 作用域包的 `@` 在开头，查找版本分隔符要从第一个 `/` 之后开始。
    let search_from = if spec.starts_with('@') {
        spec.find('/').map(|idx| idx + 1).unwrap_or(spec.len())
    } else {
        0
    };
    match spec[search_from..].find('@') {
        Some(offset) if offset > 0 => {
            let at = search_from + offset;
            let version = spec[at + 1..].trim();
            (
                spec[..at].to_string(),
                (!version.is_empty()).then(|| version.to_string()),
            )
        }
        _ => (spec.to_string(), None),
    }
}

/// `git:host/owner/repo@ref`、`https://host/owner/repo[@ref]`、`git@host:owner/repo`。
///
/// pi 用 `hosted-git-info` 做完整解析；这里覆盖配置文件里真会出现的三种写法，
/// 认不出来就当本地路径（与 pi 的 `parseSource` 兜底一致）。
fn parse_git(trimmed: &str) -> Option<Source> {
    let looks_git = trimmed.starts_with("git:")
        || trimmed.starts_with("git@")
        || trimmed.starts_with("github:")
        || trimmed.starts_with("https://")
        || trimmed.starts_with("http://")
        || trimmed.starts_with("ssh://");
    if !looks_git {
        return None;
    }

    let body = match trimmed.strip_prefix("git:") {
        Some(rest) => rest.trim().to_string(),
        None => trimmed.to_string(),
    };
    // `git@host:owner/repo`
    let body = if let Some(rest) = body.strip_prefix("git@") {
        match rest.split_once(':') {
            Some((host, path)) => format!("{host}/{path}"),
            None => return None,
        }
    } else {
        // 去掉 scheme
        ["https://", "http://", "ssh://", "git://", "github:"]
            .iter()
            .find_map(|scheme| body.strip_prefix(scheme).map(str::to_string))
            .unwrap_or(body)
    };

    let (repo, git_ref) = split_git_ref(&body);
    let repo = repo.trim_end_matches('/').to_string();
    let repo = repo.strip_suffix(".git").unwrap_or(&repo).to_string();
    let (host, path) = repo.split_once('/')?;
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(Source::Git {
        host: host.to_string(),
        path: path.to_string(),
        git_ref,
    })
}

/// URL 里最后一个 `@` 且位于最后一个 `/` 之后 = ref。
fn split_git_ref(url: &str) -> (String, Option<String>) {
    let Some(at) = url.rfind('@') else {
        return (url.to_string(), None);
    };
    let slash = url.rfind('/');
    if slash.is_some_and(|slash| slash > at) {
        return (url.to_string(), None);
    }
    let git_ref = url[at + 1..].trim();
    (
        url[..at].to_string(),
        (!git_ref.is_empty()).then(|| git_ref.to_string()),
    )
}

/// `configuredVersion`：npm 的版本段 / git 的 ref（pi 的 `getConfiguredVersion`）。
fn configured_version(source: &Source) -> Option<String> {
    match source {
        Source::Npm { version, .. } => version.clone(),
        Source::Git { git_ref, .. } => git_ref.clone(),
        Source::Local { .. } => None,
    }
}

// ============================================================================
// 安装位置
// ============================================================================

/// 包安装位置，不存在时 `None`（pi 的 `getInstalledPath`）。
///
/// 安装根：全局 = `<toilerHome>/plugins`，项目 = `<cwd>/.pi`（项目插件跟项目配置
/// 在同一个 `.pi/` 下，与 pi 一致）。
fn installed_path(source: &str, scope: Scope, cwd: &Path, plugins_root: &Path) -> Option<PathBuf> {
    let base = match scope {
        Scope::Global => plugins_root.to_path_buf(),
        Scope::Project => cwd.join(".pi"),
    };
    let path = match parse_source(source) {
        Source::Npm { name, .. } => base.join("npm").join("node_modules").join(name),
        Source::Git { host, path, .. } => base.join("git").join(host).join(path),
        Source::Local { path } => {
            let expanded = paths::expand_tilde(&path);
            if expanded.is_absolute() {
                expanded
            } else {
                base.join(expanded)
            }
        }
    };
    path.exists().then_some(path)
}

/// `package.json` 的 `(name, version)`。`installedPath` 指到文件时读同目录那份。
fn package_metadata(installed: &Path) -> (Option<String>, Option<String>) {
    let manifest = if installed.is_dir() {
        installed.join("package.json")
    } else {
        match installed.parent() {
            Some(dir) => dir.join("package.json"),
            None => return (None, None),
        }
    };
    let raw = read_json(&manifest);
    let text = |key: &str| {
        raw.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    };
    (text("name"), text("version"))
}

// ============================================================================
// 资源发现
// ============================================================================

/// 包内的资源清单，按 `Kind::ALL` 顺序、同类内按路径排序。
///
/// pi 的顺序是文件系统的读取顺序（不稳定），这里排序——前端本来就按 kind 分组，
/// 排序只是让结果可测。
fn collect_resources(package_root: &Path, filter: Option<&Value>) -> Vec<(Kind, PathBuf)> {
    let manifest = pi_manifest(package_root);
    let mut out = Vec::new();
    for kind in Kind::ALL {
        // 对象形态的过滤清单里，该类型的空数组 = 用户显式禁用它
        // （pi 的 `applyPackageFilter`：空数组让该类型的资源全部 `enabled=false`）。
        if filter
            .and_then(|filter| filter.get(kind.dir()))
            .and_then(Value::as_array)
            .is_some_and(|list| list.is_empty())
        {
            continue;
        }
        let entries: Vec<String> = manifest
            .as_ref()
            .and_then(|manifest| manifest.get(kind.dir()))
            .and_then(Value::as_array)
            .map(|list| {
                list.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let mut files: Vec<PathBuf> = Vec::new();
        if entries.is_empty() {
            // 没有 `pi` 清单时按约定目录 `<packageRoot>/<kind>`；有清单但缺这个字段
            // 就是这个包没有该类型资源，不回退目录（pi 的 `collectPackageResources`）。
            if manifest.is_none() {
                push_entry(&mut files, &package_root.join(kind.dir()), kind);
            }
        } else {
            for entry in entries {
                // 覆盖模式（`!` `+` `-`）只影响启用状态，toiler 不做启停，跳过。
                if matches!(entry.chars().next(), Some('!' | '+' | '-')) {
                    continue;
                }
                // glob 条目要展开目录树，这里不支持（见模块文档的差异表）。
                if entry.contains('*') || entry.contains('?') {
                    continue;
                }
                let resolved = if Path::new(&entry).is_absolute() {
                    PathBuf::from(&entry)
                } else {
                    package_root.join(&entry)
                };
                push_entry(&mut files, &resolved, kind);
            }
        }
        files.sort();
        files.dedup();
        for file in files {
            out.push((kind, file));
        }
    }
    out
}

/// `package.json` 的 `pi` 清单里四个资源字段。
fn pi_manifest(package_root: &Path) -> Option<Map<String, Value>> {
    let raw = read_json(&package_root.join("package.json"));
    let manifest = raw.get("pi")?.as_object()?;
    let mut out = Map::new();
    for kind in Kind::ALL {
        let Some(list) = manifest.get(kind.dir()).and_then(Value::as_array) else {
            continue;
        };
        if list.iter().all(Value::is_string) && !list.is_empty() {
            out.insert(kind.dir().into(), Value::Array(list.clone()));
        }
    }
    Some(out)
}

fn push_entry(files: &mut Vec<PathBuf>, path: &Path, kind: Kind) {
    let path = paths::absolute(path);
    if path.is_file() {
        files.push(path);
    } else if path.is_dir() {
        files.extend(collect_dir(&path, kind));
    }
}

fn collect_dir(dir: &Path, kind: Kind) -> Vec<PathBuf> {
    match kind {
        Kind::Extensions => collect_extensions(dir),
        Kind::Skills => collect_skills(dir, dir),
        Kind::Prompts => collect_files(dir, "md"),
        Kind::Themes => collect_files(dir, "json"),
    }
}

/// 目录本身是不是一个扩展（`pi.extensions` 清单 / `index.ts` / `index.js`）。
fn extension_entries(dir: &Path) -> Option<Vec<PathBuf>> {
    if let Some(manifest) = pi_manifest(dir) {
        if let Some(list) = manifest.get("extensions").and_then(Value::as_array) {
            let entries: Vec<PathBuf> = list
                .iter()
                .filter_map(Value::as_str)
                .map(|entry| {
                    if Path::new(entry).is_absolute() {
                        PathBuf::from(entry)
                    } else {
                        dir.join(entry)
                    }
                })
                .filter(|path| path.exists())
                .collect();
            if !entries.is_empty() {
                return Some(entries);
            }
        }
    }
    for candidate in ["index.ts", "index.js"] {
        let path = dir.join(candidate);
        if path.is_file() {
            return Some(vec![path]);
        }
    }
    None
}

/// 扩展发现：目录自己是一个扩展就只算它；否则直下的 `.ts|.js` 文件 + 子目录里
/// 「看起来是扩展」的入口（pi 的 `collectAutoExtensionEntries`，只深入一层）。
fn collect_extensions(dir: &Path) -> Vec<PathBuf> {
    if let Some(entries) = extension_entries(dir) {
        return entries;
    }
    let mut out = Vec::new();
    for entry in read_dir_sorted(dir) {
        if skip_name(&entry) {
            continue;
        }
        let path = dir.join(&entry);
        if path.is_file() && (entry.ends_with(".ts") || entry.ends_with(".js")) {
            out.push(path);
        } else if path.is_dir() {
            if let Some(entries) = extension_entries(&path) {
                out.extend(entries);
            }
        }
    }
    out
}

/// 技能发现（pi 的 `collectSkillEntries(dir, "pi")`）：
/// 当前目录有 `SKILL.md` 就只收它并停止下探；否则收当前目录的 `.md` 文件（仅根目录）
/// 并递归子目录。
fn collect_skills(dir: &Path, root: &Path) -> Vec<PathBuf> {
    let entries = read_dir_sorted(dir);
    let skill_file = dir.join("SKILL.md");
    if entries.iter().any(|name| name == "SKILL.md") && skill_file.is_file() {
        return vec![skill_file];
    }
    let mut out = Vec::new();
    for entry in entries {
        if skip_name(&entry) {
            continue;
        }
        let path = dir.join(&entry);
        if path.is_dir() {
            out.extend(collect_skills(&path, root));
        } else if path.is_file() && entry.ends_with(".md") && dir == root {
            out.push(path);
        }
    }
    out
}

/// 递归收集指定扩展名的文件（pi 的 `collectFiles`）。
fn collect_files(dir: &Path, extension: &str) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for entry in read_dir_sorted(dir) {
        if skip_name(&entry) {
            continue;
        }
        let path = dir.join(&entry);
        if path.is_dir() {
            out.extend(collect_files(&path, extension));
        } else if path.is_file() && entry.ends_with(&format!(".{extension}")) {
            out.push(path);
        }
    }
    out
}

/// 递归时一律跳过的条目名（pi 还会读 `.gitignore` 一族，见模块文档）。
fn skip_name(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules"
}

fn read_dir_sorted(dir: &Path) -> Vec<String> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut names: Vec<String> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().to_string())
        .collect();
    names.sort();
    names
}

/// 资源显示名（pi 的 `getResourceName`）。
fn resource_name(path: &Path, kind: Kind) -> String {
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default();
    let lower = file.to_ascii_lowercase();
    if kind == Kind::Skills && lower == "skill.md" {
        return parent_name(path);
    }
    match kind {
        Kind::Extensions if lower == "index.ts" || lower == "index.js" => parent_name(path),
        Kind::Extensions | Kind::Prompts | Kind::Themes => stem(&file),
        Kind::Skills => file,
    }
}

fn parent_name(path: &Path) -> String {
    path.parent()
        .and_then(|parent| parent.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn stem(file: &str) -> String {
    match file.rsplit_once('.') {
        Some((base, _)) if !base.is_empty() => base.to_string(),
        _ => file.to_string(),
    }
}

/// 包根下的相对路径；算不出来（符号链接逃逸等）就退回绝对路径（同 pi）。
fn relative_from(root: &Path, path: &Path) -> Option<String> {
    let rel = path.strip_prefix(root).ok()?;
    let text = posix(rel);
    (!text.is_empty() && !text.starts_with("..")).then_some(text)
}

fn posix(path: &Path) -> String {
    path.to_string_lossy().replace('\\', "/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_spec_keeps_scope_and_splits_version() {
        assert_eq!(
            split_npm_spec("@scope/pkg@^1.2.0"),
            ("@scope/pkg".to_string(), Some("^1.2.0".to_string()))
        );
        assert_eq!(
            split_npm_spec("pkg@1.0.0"),
            ("pkg".to_string(), Some("1.0.0".into()))
        );
        assert_eq!(split_npm_spec("pkg"), ("pkg".to_string(), None));
        assert_eq!(
            split_npm_spec("@scope/pkg"),
            ("@scope/pkg".to_string(), None)
        );
    }

    #[test]
    fn git_sources_resolve_host_path_and_ref() {
        let Source::Git {
            host,
            path,
            git_ref,
        } = parse_source("git:github.com/me/repo@v1")
        else {
            panic!("git: 前缀要认成 git 来源");
        };
        assert_eq!((host.as_str(), path.as_str()), ("github.com", "me/repo"));
        assert_eq!(git_ref.as_deref(), Some("v1"));

        let Source::Git {
            host,
            path,
            git_ref,
        } = parse_source("https://github.com/me/repo.git")
        else {
            panic!("https 要认成 git 来源");
        };
        assert_eq!((host.as_str(), path.as_str()), ("github.com", "me/repo"));
        assert_eq!(git_ref, None);

        let Source::Git { host, path, .. } = parse_source("git@github.com:me/repo.git") else {
            panic!("scp 风格要认成 git 来源");
        };
        assert_eq!((host.as_str(), path.as_str()), ("github.com", "me/repo"));
    }

    #[test]
    fn local_source_is_the_fallback() {
        assert!(matches!(
            parse_source("./plugins/demo"),
            Source::Local { .. }
        ));
    }

    #[test]
    fn disabled_entry_needs_all_four_empty_arrays() {
        assert!(is_disabled_entry(&json!({
            "source": "npm:x",
            "extensions": [],
            "skills": [],
            "prompts": [],
            "themes": [],
        })));
        assert!(!is_disabled_entry(&json!({
            "source": "npm:x",
            "extensions": [],
            "skills": [],
            "prompts": [],
        })));
        assert!(!is_disabled_entry(&json!({
            "source": "npm:x",
            "extensions": ["./a.ts"],
            "skills": [],
            "prompts": [],
            "themes": [],
        })));
    }
}
