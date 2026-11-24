//! 技能面板与 skills.sh 市场：`GET /api/skills`、搜索和技能包管理。
//!
//! **扫描规则与加载逻辑全在 [`crate::skills`]**：目录约定、项目根、信任前提、
//! 去重规则都写在那里，因为系统提示词的 `<skills>` 段（`crate::system_prompt`）
//! 必须与这个面板看到同一份技能。这个模块还负责把 skills.sh 的搜索结果投影成
//! 旧 pi 前端需要的 `{package, installs, url}` 形状，并转交安装器写入本地目录。
//!
//! # 与 pi 的差异
//!
//! | 项 | pi | toiler |
//! |---|---|---|
//! | 技能来源 | `DefaultResourceLoader`：`skillPaths` + package 技能 + `.agents/skills` | 只扫 `crate::skills::dirs` 那几处（无 package / `skillPaths`） |
//! | 市场搜索 | pi 的 server route | 读取 skills.sh 的公开 `/api/search` |
//! | 市场安装 | 执行 `npx skills` | Rust 下载 GitHub 归档，只解出选中的技能目录，不依赖 Node |
//! | 开关 | `PATCH /api/skills` 改 `disable-model-invocation` | 仍未开放写 `SKILL.md` |
//!
//! skills.sh 官方 `/api/v1` 目录接口要求 Vercel OIDC，不能由本地桌面应用伪造；
//! `/api/search` 是网站搜索页实际使用的公开接口。安装结果会写来源元数据，列表
//! 据此显示 `skills.sh` 链接，但元数据文件不会被技能加载器当作技能。

use std::path::{Path, PathBuf};

use axum::extract::{Json, Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use rpi_harness::skills::SkillDiagnostic;
use rpi_harness::types::Skill;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::paths;
use crate::security::Trusted;
use crate::skills as discovery;
use crate::skills_market::{self as market, MarketError, Scope};
use crate::state::AppState;

/// `GET /api/skills?cwd=<绝对路径>`。
///
/// `?cache=1` 是 pi 的「只读缓存」开关。toiler 没有技能缓存，参数被忽略
/// （axum 默认丢弃未知 query 字段），响应里 `cacheHit` 恒为 `false`；
/// 前端的预填逻辑只在 `cacheHit === true` 时采用，因此不会误显示旧数据。
#[derive(Debug, Deserialize)]
pub struct SkillsQuery {
    /// 项目目录。pi 缺省时返回 400，这里保持一致。
    cwd: Option<String>,
}

pub async fn list(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(query): Query<SkillsQuery>,
) -> Response {
    let raw_cwd = query.cwd.unwrap_or_default();
    let cwd = raw_cwd.trim();
    if cwd.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({ "error": "cwd required" })),
        )
            .into_response();
    }

    // 授权与 `/api/files` 同两层：先词法包含，再 realpath 包含（挡符号链接逃逸）。
    // `sync_allowed_roots` 不能省：允许根是每次判定前重建的（默认 cwd + 会话 cwd +
    // 已存项目），不重建时集合是空的，任何 cwd 都会 403。
    state.sync_allowed_roots();
    let cwd_path = paths::absolute(Path::new(cwd));
    if !state.roots().allows(&cwd_path) || !state.roots().allows_real(&cwd_path) {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({ "error": "Access denied" })),
        )
            .into_response();
    }

    // 项目技能要不要加载取决于项目是否被信任，先算出来（与 pi 的 projectTrusted 同义）。
    // 前端拿同一个布尔值禁用「Project」作用域按钮，两边语义必须一致。
    let trusted = crate::trust::status(state.project_trust(), cwd).trusted;

    // 扫描规则在 `crate::skills`：系统提示词的 `<skills>` 段走同一份函数，
    // 面板上看到的技能与模型看到的技能因此永远不会不一致。
    let found = discovery::load(&cwd_path, trusted).await;

    let skills: Vec<Value> = found
        .skills
        .iter()
        .map(|loaded| {
            let mut value =
                skill_json(&loaded.skill, loaded.origin.source(), loaded.origin.scope());
            let scope = match loaded.origin {
                discovery::Origin::User => Scope::Global,
                discovery::Origin::Project => Scope::Project,
            };
            if let Some(install) = market::metadata_for_skill_file(
                Path::new(&loaded.skill.file_path),
                &cwd_path,
                scope,
            ) {
                value["install"] = json!(install);
            }
            value
        })
        .collect();
    let diagnostics: Vec<Value> = found
        .warnings
        .iter()
        .map(|warning| diagnostic_json(&warning.diagnostic, warning.origin.source()))
        .collect();

    // 前端用 `projectResourcesLoaded === false` 禁用「Project」作用域按钮：
    // 未信任的项目不该把它的技能当成可用资源（与 pi 的 getProjectTrustStatus 同义）。
    Json(json!({
        "skills": skills,
        "diagnostics": diagnostics,
        "projectResourcesLoaded": trusted,
        "cacheHit": false,
    }))
    .into_response()
}

/// `POST /api/skills/search`：从 skills.sh 的公开搜索接口读取市场数据。
#[derive(Debug, Deserialize)]
pub struct MarketSearchRequest {
    query: String,
}

pub async fn market_search(
    _trusted: Trusted,
    Json(request): Json<MarketSearchRequest>,
) -> Response {
    match crate::skills_market::search(&request.query).await {
        Ok(results) => Json(json!({ "results": results })).into_response(),
        Err(error) => (error.status, Json(json!({ "error": error.message }))).into_response(),
    }
}

/// `POST /api/skills/install`：下载并安装搜索结果里的一个 GitHub 技能。
#[derive(Debug, Deserialize)]
pub struct MarketInstallRequest {
    package: String,
    scope: String,
    cwd: String,
}

pub async fn market_install(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(request): Json<MarketInstallRequest>,
) -> Response {
    let result = async {
        let scope = Scope::parse(&request.scope)?;
        let (cwd, _) = authorize_market(&state, &request.cwd, Some(scope))?;
        market::install(&request.package, scope, &cwd).await
    }
    .await;
    market_response(result)
}

#[derive(Debug, Deserialize)]
pub struct MarketCheckRequest {
    cwd: String,
    scope: Option<String>,
    package: Option<String>,
}

pub async fn market_check(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(request): Json<MarketCheckRequest>,
) -> Response {
    let result = async {
        let scope = request.scope.as_deref().map(Scope::parse).transpose()?;
        let (cwd, trusted) = authorize_market(&state, &request.cwd, scope)?;
        market::check(request.package.as_deref(), scope, &cwd, trusted).await
    }
    .await;
    market_response(result)
}

pub async fn market_update(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(request): Json<MarketInstallRequest>,
) -> Response {
    let result = async {
        let scope = Scope::parse(&request.scope)?;
        let (cwd, _) = authorize_market(&state, &request.cwd, Some(scope))?;
        market::update(&request.package, scope, &cwd).await
    }
    .await;
    market_response(result)
}

pub async fn market_remove(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(request): Json<MarketInstallRequest>,
) -> Response {
    let result = async {
        let scope = Scope::parse(&request.scope)?;
        let (cwd, _) = authorize_market(&state, &request.cwd, Some(scope))?;
        market::remove(&request.package, scope, &cwd).await
    }
    .await;
    market_response(result)
}

fn authorize_market(
    state: &AppState,
    cwd: &str,
    scope: Option<Scope>,
) -> Result<(PathBuf, bool), MarketError> {
    let cwd = cwd.trim();
    let path = Path::new(cwd);
    if !path.is_absolute() {
        return Err(MarketError {
            status: StatusCode::BAD_REQUEST,
            message: "cwd 必须是绝对路径".into(),
        });
    }
    let cwd_path = paths::absolute(path);
    state.sync_allowed_roots();
    if !state.roots().allows(&cwd_path) || !state.roots().allows_real(&cwd_path) {
        return Err(MarketError {
            status: StatusCode::FORBIDDEN,
            message: "Access denied".into(),
        });
    }
    if !cwd_path.is_dir() {
        return Err(MarketError {
            status: StatusCode::BAD_REQUEST,
            message: "cwd 必须是已存在的目录".into(),
        });
    }
    let trusted =
        crate::trust::status(state.project_trust(), cwd_path.to_string_lossy().as_ref()).trusted;
    if scope == Some(Scope::Project) && !trusted {
        return Err(MarketError {
            status: StatusCode::FORBIDDEN,
            message: "Project resources require trust".into(),
        });
    }
    Ok((cwd_path, trusted))
}

fn market_response(result: Result<Value, MarketError>) -> Response {
    match result {
        Ok(result) => Json(result).into_response(),
        Err(error) => (error.status, Json(json!({ "error": error.message }))).into_response(),
    }
}

/// `PATCH /api/skills`：切换 `disable-model-invocation`。
pub async fn toggle_not_implemented() -> Response {
    not_implemented("暂不支持修改禁用模型调用：该操作会改写 SKILL.md")
}

fn not_implemented(message: &str) -> Response {
    (
        StatusCode::NOT_IMPLEMENTED,
        Json(json!({ "error": message })),
    )
        .into_response()
}

fn skill_json(skill: &Skill, source: &str, scope: &str) -> Value {
    // 技能文件路径可能来自 `canonicalize`（Windows 上是 `\\?\C:\…`），面板上
    // 直接显示给用户，先去掉扩展前缀。
    let file_path = crate::paths::simplified(&skill.file_path);
    let base_dir = Path::new(&file_path)
        .parent()
        .map(|parent| crate::paths::simplified(&parent.to_string_lossy()))
        .unwrap_or_default();
    json!({
        "name": skill.name,
        "description": skill.description,
        "filePath": file_path,
        "baseDir": base_dir,
        "disableModelInvocation": skill.disable_model_invocation.unwrap_or(false),
        "sourceInfo": { "source": source, "scope": scope },
    })
}

/// 前端按 `{type, message, source}` 渲染（`type === "error"` 才标红），
/// `rpi` 的诊断没有 `type` 字段，统一记为 `warning` —— 与 pi 的
/// `ResourceDiagnostic`（技能加载失败只警告、不阻断）对齐。
fn diagnostic_json(diagnostic: &SkillDiagnostic, source: &str) -> Value {
    json!({
        "type": "warning",
        "code": diagnostic.code.as_str(),
        "message": diagnostic.message,
        "path": diagnostic.path,
        "source": source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skill_json_carries_every_field_the_panel_reads() {
        // 前端 `SkillsConfig` 按这六个字段渲染；少一个整块不渲染或落到 Other 组。
        let value = skill_json(
            &Skill {
                name: "demo".to_string(),
                description: "说明".to_string(),
                content: "正文".to_string(),
                file_path: "/p/.agents/skills/demo/SKILL.md".to_string(),
                disable_model_invocation: Some(true),
            },
            "project",
            "project",
        );
        assert_eq!(value["name"], "demo");
        assert_eq!(value["description"], "说明");
        assert_eq!(value["baseDir"], "/p/.agents/skills/demo");
        assert_eq!(value["disableModelInvocation"], true);
        assert_eq!(value["sourceInfo"]["source"], "project");
        assert_eq!(value["sourceInfo"]["scope"], "project");
    }

    #[test]
    fn diagnostics_are_warnings_carrying_the_origin() {
        let value = diagnostic_json(
            &SkillDiagnostic {
                code: rpi_harness::skills::SkillDiagnosticCode::ParseFailed,
                message: "坏掉的 frontmatter".to_string(),
                path: "/p/SKILL.md".to_string(),
            },
            "user",
        );
        assert_eq!(value["type"], "warning");
        assert_eq!(value["code"], "parse_failed");
        assert_eq!(value["source"], "user");
    }
}
