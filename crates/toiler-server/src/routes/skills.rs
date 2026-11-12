//! 技能面板：`GET /api/skills`（列表 + 诊断 + 项目资源是否加载）。
//!
//! pi 的对应实现是 `app/api/skills/route.ts` + `lib/skills-service.ts`：
//! 用 `DefaultResourceLoader` 扫 `settings.json` 的 `skillPaths`、package 自带
//! 的技能，以及 `.agents/skills`。workman 没有 package 系统，`settings.json`
//! 里也没有 `skillPaths` 键，因此只扫 pi 在默认配置下真正会命中的那一对目录：
//!
//! | 目录 | `sourceInfo` | 前端分组 |
//! |---|---|---|
//! | `<项目根>/.agents/skills`（项目根 = 最近的 git 仓库根，否则 cwd 本身） | `{source:"project", scope:"project"}` | Project |
//! | `~/.agents/skills` | `{source:"user", scope:"user"}` | Global |
//! | `~/.pi/agent/skills`（pi 的 agent 目录） | 同上 | Global |
//! | `~/.agents/workman/skills`（workman 的 agent 目录） | 同上 | Global |
//!
//! # 项目技能的两个前提
//!
//! 1. **只认项目根这一层**：约定位置是 `$project/.agents/skills`。项目根取
//!    「从 cwd 向上最近的 git 仓库根」，不在 git 里就是 cwd 自己。所以子目录里
//!    开会话（`$project/frontend`）也能看到项目根的技能，而仓库之外的父目录
//!    不会被误认成项目。
//! 2. **项目已信任**：未信任时 pi 根本不加载项目技能（`projectTrusted ?
//!    collectAncestorAgentsSkillDirs(...) : []`），这里保持一致——否则一个
//!    刚 clone 下来的仓库可以靠 `.agents/skills` 里的提示词影响 agent。
//!
//! 这两个条件也解释了为什么「不同项目会有不同的技能」，以及为什么刚建的
//! 项目目录里技能不会马上出现（需要先在设置里信任它）。
//!
//! # 与 pi 的差异
//!
//! | 项 | pi | workman |
//! |---|---|---|
//! | 技能来源 | `DefaultResourceLoader`：`skillPaths` + package 技能 + 上述两目录 | 只扫上述两目录（无 package / `skillPaths`） |
//! | 同名冲突 | loader 按 `name` 去重，项目版本把用户版本顶掉 | 按 `filePath` 去重，**同名不同文件两份都返回**（项目在前） |
//! | 缓存 | `pi.sqlite` 的 `skill_cache`，`?cache=1` 可命中 | 不缓存；`cacheHit` 恒 `false`，前端未命中会照常走全量请求 |
//! | 使用统计 | `skill_usage` 表补 `usageCount` / `lastUsedAt` | 不提供（前端只在排序/展示时读，缺失即不显示） |
//! | 技能市场 | `/api/skills/search`、`/api/skills/install` 可用 | 501 + 中文说明（PLAN 已明确砍掉技能市场） |
//! | 开关 | `PATCH /api/skills` 改 `disable-model-invocation` | 501（要改写用户目录里的 `SKILL.md`，暂不开放写） |
//!
//! 注意两处「静默」行为，排查问题时最容易被误导：
//!
//! - `SKILL.md` **缺 `description`** 时 `rpi` 直接丢弃、**不产生诊断**（与 pi 一致）。
//!   技能写好了却不出现，先查 frontmatter。
//! - 诊断只覆盖读取/解析失败；目录不存在不算错误（见 [`skill_dirs`] 直接跳过）。

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use rpi_harness::skills::{load_skills, SkillDiagnostic};
use rpi_harness::types::Skill;
use rpi_tools::{ExecutionEnv, OsExecutionEnv};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::paths;
use crate::security::Trusted;
use crate::state::AppState;

/// `GET /api/skills?cwd=<绝对路径>`。
///
/// `?cache=1` 是 pi 的「只读缓存」开关。workman 没有技能缓存，参数被忽略
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

    let env: Arc<dyn ExecutionEnv> = Arc::new(OsExecutionEnv::with_cwd(cwd_path.clone()));

    let mut seen_paths: HashSet<String> = HashSet::new();
    // 用户级目录有两份副本很容易重名（`~/.agents/skills` 与 `~/.pi/agent/skills`
    // 常常放着同一个技能），同名只留先扫到的那份；项目级不参与这个去重。
    let mut seen_user_names: HashSet<String> = HashSet::new();
    let mut skills: Vec<Value> = Vec::new();
    let mut diagnostics: Vec<Value> = Vec::new();

    for (dir, source, scope) in skill_dirs(&cwd_path, trusted) {
        let dir = dir.to_string_lossy().to_string();
        let loaded = load_skills(&env, std::slice::from_ref(&dir)).await;
        for diagnostic in &loaded.diagnostics {
            diagnostics.push(diagnostic_json(diagnostic, source));
        }
        for skill in loaded.skills {
            if source == "user" && !seen_user_names.insert(skill.name.clone()) {
                continue;
            }
            push_skill(
                &mut skills,
                &mut seen_paths,
                skill_json(&skill, source, scope),
            );
        }
    }

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
    not_implemented("技能市场未实现：workman 只加载本地 ~/.agents/skills 与项目 .agents/skills")
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

/// 要扫的技能目录，**按优先级排序**（先出现的胜出），且只返回真实存在的目录。
///
/// 顺序 = 项目（从 cwd 向外，最近的最优先）→ 用户级三个目录。
/// 不存在的目录直接跳过而不是交给 `load_skills`：后者会为它产出 `list_failed`
/// 诊断，而「没装过项目技能」是常态，不该在界面上报 warning。
fn skill_dirs(cwd: &Path, trusted: bool) -> Vec<(PathBuf, &'static str, &'static str)> {
    let mut out = Vec::new();
    let user_agents = paths::agents_home().join("skills");

    if trusted {
        // 只认**项目根**这一层：`$project/.agents/skills`。
        // 项目根 = 从 cwd 向上最近的 git 仓库根；不在 git 里就是 cwd 自己。
        // 与 pi 的差异：pi 的 `collectAncestorAgentsSkillDirs` 在找不到 git 根时
        // 会一路扫到文件系统根，那样 `~/Development/.agents/skills` 这类「祖先共享
        // 目录」也会被当成项目技能。workman 只认项目根，行为更好预测。
        if let Some(root) = project_root(cwd) {
            let candidate = root.join(".agents").join("skills");
            // `~/.agents/skills` 在下面按 user 加，别重复标成 project。
            if candidate != user_agents {
                push_dir(&mut out, candidate, "project", "project");
            }
        }
    }

    // 用户级：跨工具目录 + pi 的 agent 目录 + workman 自己的 agent 目录。
    // 两个 agent 目录都算，是为了让「刚从 pi 迁过来」和「只在 workman 里建过」
    // 的技能都能看到——pi 的 user skills 默认就在 `~/.pi/agent/skills`。
    push_dir(&mut out, user_agents, "user", "user");
    push_dir(
        &mut out,
        paths::legacy_pi_agent_dir().join("skills"),
        "user",
        "user",
    );
    push_dir(
        &mut out,
        paths::workman_home().join("skills"),
        "user",
        "user",
    );

    out
}

fn push_dir(
    out: &mut Vec<(PathBuf, &'static str, &'static str)>,
    dir: PathBuf,
    source: &'static str,
    scope: &'static str,
) {
    if dir.is_dir() && !out.iter().any(|(existing, _, _)| *existing == dir) {
        out.push((dir, source, scope));
    }
}

/// 项目根：从 cwd 向上最近的 git 仓库根；不在任何 git 仓库里时就是 cwd 自己。
///
/// 「项目根」而不是「cwd」是刻意的：用户在子目录里开会话（`$project/frontend`）
/// 时，`$project/.agents/skills` 仍然要生效。
fn project_root(cwd: &Path) -> Option<PathBuf> {
    let mut dir = Some(cwd.to_path_buf());
    while let Some(current) = dir {
        if current.join(".git").exists() {
            return Some(current);
        }
        dir = current.parent().map(Path::to_path_buf);
    }
    Some(cwd.to_path_buf())
}

/// 合并项目技能与用户技能。
///
/// 去重规则一句话：**项目与用户之间合并（同名都留），用户目录之间按名字去重**。
///
/// - 与 pi 的差异：pi 的 loader 用 `name` 做 key，项目里放一份同名技能会把全局
///   那份顶掉。workman 两份都返回 —— 项目技能本来就是「给这个项目换一套行为」，
///   界面上应该能同时看到、分别查看。
/// - 用户级那三个目录之间仍按名字去重，否则同一份技能（`~/.agents/skills` 与
///   `~/.pi/agent/skills` 里的副本）会在列表里出现两遍。
/// - 同一个文件被扫到两次才按 `filePath` 去重。
fn push_skill(out: &mut Vec<Value>, seen_paths: &mut HashSet<String>, skill: Value) {
    let path = skill["filePath"].as_str().unwrap_or_default().to_string();
    if seen_paths.insert(path) {
        out.push(skill);
    }
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
    fn same_name_from_two_sources_are_both_kept() {
        let mut out: Vec<Value> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        push_skill(
            &mut out,
            &mut seen,
            json!({ "name": "design-taste-frontend", "filePath": "/project/.agents/skills/x/SKILL.md" }),
        );
        push_skill(
            &mut out,
            &mut seen,
            json!({ "name": "design-taste-frontend", "filePath": "/home/.agents/skills/x/SKILL.md" }),
        );
        assert_eq!(out.len(), 2, "同名不同文件要合并展示");

        // 同一个文件被扫到两次只留一份
        push_skill(
            &mut out,
            &mut seen,
            json!({ "name": "design-taste-frontend", "filePath": "/project/.agents/skills/x/SKILL.md" }),
        );
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn user_dirs_dedupe_by_name() {
        // 用户级两个目录里同名 -> 只保留先扫到的那份（这里靠调用方维护 seen_user_names）
        let mut seen_user_names: HashSet<String> = HashSet::new();
        assert!(seen_user_names.insert("apple-design".to_string()));
        assert!(!seen_user_names.insert("apple-design".to_string()));
    }
}
