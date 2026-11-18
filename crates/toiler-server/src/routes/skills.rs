//! 技能面板：`GET /api/skills`（列表 + 诊断 + 项目资源是否加载）。
//!
//! **扫描规则与加载逻辑全在 [`crate::skills`]**：目录约定、项目根、信任前提、
//! 去重规则都写在那里，因为系统提示词的 `<skills>` 段（`crate::system_prompt`）
//! 必须与这个面板看到同一份技能。这个模块只负责把它投影成 pi 的 JSON 形状。
//!
//! # 与 pi 的差异
//!
//! | 项 | pi | toiler |
//! |---|---|---|
//! | 技能来源 | `DefaultResourceLoader`：`skillPaths` + package 技能 + `.agents/skills` | 只扫 `crate::skills::dirs` 那几处（无 package / `skillPaths`） |
//! | 同名冲突 | loader 按 `name` 去重，项目版本把用户版本顶掉 | 项目与用户**同名都留**（项目在前），用户目录之间按名字去重 |
//! | 缓存 | `pi.sqlite` 的 `skill_cache`，`?cache=1` 可命中 | 不缓存；`cacheHit` 恒 `false`，前端未命中会照常走全量请求 |
//! | 使用统计 | `skill_usage` 表补 `usageCount` / `lastUsedAt` | 不提供（前端只在排序/展示时读，缺失即不显示） |
//! | 技能市场 | `/api/skills/search`、`/api/skills/install` 可用 | 501 + 中文说明（PLAN 已明确砍掉技能市场） |
//! | 开关 | `PATCH /api/skills` 改 `disable-model-invocation` | 501（要改写用户目录里的 `SKILL.md`，暂不开放写） |

use std::path::Path;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rpi_harness::skills::SkillDiagnostic;
use rpi_harness::types::Skill;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::paths;
use crate::security::Trusted;
use crate::skills as discovery;
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
        .map(|loaded| skill_json(&loaded.skill, loaded.origin.source(), loaded.origin.scope()))
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

/// `POST /api/skills/search` / `POST /api/skills/install`：技能市场。
///
/// 返回 501 而不是 404，前端会把 `error` 原文显示出来，用户能看懂为什么没反应。
pub async fn market_not_implemented() -> Response {
    not_implemented("技能市场未实现：toiler 只加载本地 ~/.agents/skills 与项目 .agents/skills")
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
    let base_dir = Path::new(&skill.file_path)
        .parent()
        .map(|parent| parent.to_string_lossy().to_string())
        .unwrap_or_default();
    json!({
        "name": skill.name,
        "description": skill.description,
        "filePath": skill.file_path,
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
