//! `/api/git*` 与 `/api/worktrees` —— Git 面板与 worktree 管理。
//!
//! | route | 行为 |
//! |---|---|
//! | `GET  /api/git?cwd=` | 完整仓库状态（分支/标签/上游/增删/未跟踪…） |
//! | `POST /api/git` | 24 个动作之一（pi 的 20 个 + 标签相关的 4 个），返回操作后的状态 |
//! | `GET  /api/git/status?cwd=` | 只要「变更文件 + 增删行数」 |
//! | `GET  /api/git/diff?cwd=&path=` | 单文件补丁 |
//! | `POST /api/git/commit-message` | 暂存区 diff → 模型生成 commit message（NDJSON 流） |
//! | `GET/POST/DELETE /api/worktrees` | 列出 / 新建（或 checkout） / 删除 worktree |
//!
//! 所有 git 调用都经 [`crate::gitsvc`]，路径要过「允许根」校验（`Access denied` → 403）。
//!
//! 凭据（`credential` / `rememberCredential`）只做形状校验，真正的注入与落盘在
//! [`crate::gitsvc`] 的 `run_remote_action`：面板留空时它会自己取已加密保存的那份。

use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use futures::Stream;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::gitsvc;
use crate::security::Trusted;
use crate::state::AppState;

#[derive(Debug, Deserialize, Default)]
pub struct CwdQuery {
    cwd: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
pub struct DiffQuery {
    cwd: Option<String>,
    path: Option<String>,
}

fn git_error(status: u16, message: &str) -> Response {
    let code = StatusCode::from_u16(status).unwrap_or(StatusCode::BAD_REQUEST);
    (code, Json(json!({ "error": message }))).into_response()
}

/// `GET /api/git?cwd=` —— 面板状态。
pub async fn status(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(query): Query<CwdQuery>,
) -> Response {
    state.sync_allowed_roots();
    let cwd = match gitsvc::resolve_cwd(query.cwd.as_deref(), state.roots()) {
        Ok(path) => path,
        Err((status, message)) => return git_error(status, &message),
    };
    match blocking_plain(move || gitsvc::full_status(&cwd)).await {
        Ok(value) => Json(value).into_response(),
        Err(message) => git_error(500, &message),
    }
}

/// `POST /api/git` —— 动作。
pub async fn command(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<Value>,
) -> Response {
    state.sync_allowed_roots();
    let cwd_raw = body.get("cwd").and_then(Value::as_str);
    let cwd = match gitsvc::resolve_cwd(cwd_raw, state.roots()) {
        Ok(path) => path,
        Err((status, message)) => return git_error(status, &message),
    };
    // 借 credential 结构做一次前置校验（pi 也是先校验再执行）
    if let Some(credential) = body.get("credential") {
        if !credential.is_null() {
            let kind = credential.get("kind").and_then(Value::as_str).unwrap_or("");
            let secret = credential
                .get("secret")
                .and_then(Value::as_str)
                .unwrap_or("");
            let username_ok = matches!(
                credential.get("username"),
                None | Some(Value::Null) | Some(Value::String(_))
            );
            if (kind != "https" && kind != "ssh") || secret.is_empty() || !username_ok {
                return git_error(400, "Invalid Git credential");
            }
        }
    }
    match blocking_git(move || gitsvc::action(&cwd, &body)).await {
        Ok(value) => Json(value).into_response(),
        Err(message) => git_error(400, &message),
    }
}

/// `GET /api/git/status?cwd=`
pub async fn status_only(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(query): Query<CwdQuery>,
) -> Response {
    state.sync_allowed_roots();
    let Some(raw) = query
        .cwd
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return git_error(400, "cwd must be an absolute path");
    };
    if !crate::paths::is_absolute_path(raw) {
        return git_error(400, "cwd must be an absolute path");
    }
    if !state.roots().allows(raw) {
        return git_error(403, "Access denied");
    }
    let cwd = PathBuf::from(raw);
    let Ok(meta) = std::fs::metadata(&cwd) else {
        return git_error(404, "Directory not found");
    };
    if !meta.is_dir() {
        return git_error(400, "Not a directory");
    }
    if !state.roots().allows_real(&cwd) {
        return git_error(403, "Access denied");
    }
    match blocking_plain(move || gitsvc::status(&cwd)).await {
        Ok(value) => Json(value).into_response(),
        Err(message) => git_error(500, &message),
    }
}

/// `GET /api/git/diff?cwd=&path=`
pub async fn diff(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(query): Query<DiffQuery>,
) -> Response {
    state.sync_allowed_roots();
    let cwd_raw = query.cwd.as_deref().unwrap_or("").trim().to_string();
    let path_raw = query.path.as_deref().unwrap_or("").trim().to_string();
    if cwd_raw.is_empty() || !crate::paths::is_absolute_path(&cwd_raw) {
        return git_error(400, "cwd must be an absolute path");
    }
    if path_raw.is_empty() || !crate::paths::is_absolute_path(&path_raw) {
        return git_error(400, "path must be an absolute path");
    }
    if !state.roots().allows(&cwd_raw)
        || !state.roots().allows(&path_raw)
        || !state.roots().allows_real(&cwd_raw)
    {
        return git_error(403, "Access denied");
    }
    let cwd = PathBuf::from(&cwd_raw);
    let path = PathBuf::from(&path_raw);
    match blocking_plain(move || gitsvc::diff_for_file(&cwd, &path)).await {
        Ok(value) => Json(value).into_response(),
        Err(message) => git_error(500, &message),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct CommitMessageBody {
    cwd: Option<String>,
    locale: Option<String>,
    /// 前端会带上，但 toiler 直接用默认模型生成（见模块文档）
    #[serde(rename = "sessionId")]
    #[allow(dead_code)]
    session_id: Option<String>,
}

/// `POST /api/git/commit-message` —— NDJSON 流：`update` 增量 / `done` / `error`。
pub async fn commit_message(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<CommitMessageBody>,
) -> Response {
    state.sync_allowed_roots();
    let cwd = match gitsvc::resolve_cwd(body.cwd.as_deref(), state.roots()) {
        Ok(path) => path,
        Err((status, message)) => return git_error(status, &message),
    };
    let locale = if body.locale.as_deref() == Some("zh-CN") {
        "zh-CN"
    } else {
        "en"
    };

    let diff = match blocking_git(move || gitsvc::staged_diff(&cwd)).await {
        Ok(diff) => diff,
        Err(message) => {
            // pi 对暂存区 diff 的失败一律 400（「Not a Git repository」也是）
            return git_error(400, &message);
        }
    };

    let Some((provider_id, model_id)) = state.default_model_ref() else {
        return git_error(
            400,
            "No available model configured for commit-message generation",
        );
    };
    let Some(model) = state.catalog().model_for(&provider_id, &model_id).cloned() else {
        return git_error(
            400,
            "No available model configured for commit-message generation",
        );
    };
    let Some(runtime) = state
        .catalog()
        .providers
        .iter()
        .find(|p| p.id() == provider_id.as_str())
        .cloned()
    else {
        return git_error(
            400,
            "No available model configured for commit-message generation",
        );
    };

    let instructions = if locale == "zh-CN" {
        "你是 Git 提交信息助手。根据下面的 diff 写一条简洁的提交信息。\n\
         - 第一行不超过 72 个字符，格式 `<type>(<scope>): <summary>`\n\
         - 需要时在空行后补充要点，每行以 `- ` 开头\n\
         - 只输出提交信息本身，不要代码块、不要解释"
            .to_string()
    } else {
        "You write Git commit messages. Summarize the diff below.\n\
         - First line at most 72 characters, `<type>(<scope>): <summary>`\n\
         - Optionally add bullet points after a blank line\n\
         - Output only the commit message, no code fences, no explanation"
            .to_string()
    };
    let prompt = format!("{instructions}\n\n```diff\n{diff}\n```");
    let api_key = resolve_key(&state, &provider_id);

    let stream = async_stream::stream! {
        let result = crate::oneshot::complete(
            runtime,
            &model,
            api_key,
            prompt,
            1024,
            std::time::Duration::from_secs(90),
        )
        .await;
        match result {
            Ok(reply) => {
                let text = reply.text.trim().to_string();
                yield Ok::<bytes::Bytes, std::convert::Infallible>(bytes::Bytes::from(format!(
                    "{}\n",
                    json!({ "type": "update", "message": text })
                )));
                yield Ok(bytes::Bytes::from(format!(
                    "{}\n",
                    json!({ "type": "done", "message": text })
                )));
            }
            Err(e) => {
                yield Ok(bytes::Bytes::from(format!(
                    "{}\n",
                    json!({ "type": "error", "error": e.to_string() })
                )));
            }
        }
    };

    ndjson(stream)
}

/// NDJSON（`application/x-ndjson`）：每行一个 JSON。
fn ndjson<S>(stream: S) -> Response
where
    S: Stream<Item = Result<bytes::Bytes, std::convert::Infallible>> + Send + 'static,
{
    let body = axum::body::Body::from_stream(stream);
    let mut response = Response::new(body);
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/x-ndjson; charset=utf-8"),
    );
    response.headers_mut().insert(
        axum::http::header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-store, no-transform"),
    );
    response.headers_mut().insert(
        "x-accel-buffering",
        axum::http::HeaderValue::from_static("no"),
    );
    response
}

fn resolve_key(state: &AppState, provider: &str) -> Option<String> {
    if let Some(key) = state.auth().api_key(provider) {
        return Some(key);
    }
    let root = state.models_file().read();
    let config = crate::models_json::provider_config(&root, provider)?;
    match crate::auth::resolve_key_config(config.get("apiKey")?) {
        crate::auth::ResolvedKey::Literal(key) => Some(key),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// worktrees
// ---------------------------------------------------------------------------

/// `GET /api/worktrees?cwd=`
pub async fn worktrees_list(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(query): Query<CwdQuery>,
) -> Response {
    state.sync_allowed_roots();
    let Some(raw) = query.cwd.as_deref().filter(|s| !s.is_empty()) else {
        return git_error(400, "cwd is required");
    };
    if !state.roots().allows(raw) || !state.roots().allows_real(raw) {
        return git_error(403, "Access denied");
    }
    let cwd = raw.to_string();
    match blocking_plain(move || {
        let info = gitsvc::repo_info(&cwd);
        let root = info.project_root.clone();
        let path = PathBuf::from(&root);
        let (list, local, remote) = if path.exists() {
            (
                gitsvc::worktrees(&path),
                gitsvc::local_branches(&path),
                gitsvc::remote_branches(&path),
            )
        } else {
            (Vec::new(), Vec::new(), Vec::new())
        };
        let is_git = !list.is_empty();
        let current = gitsvc::current_worktree_path(&list, Path::new(&cwd));
        json!({
            "projectRoot": root,
            "projectKey": crate::paths::project_key(&root),
            "isGit": is_git,
            "isTopLevel": info.is_top_level,
            "currentWorktreePath": current,
            "worktrees": list.iter().map(|wt| json!({
                "path": wt.path,
                "branch": wt.branch,
                "isMain": wt.is_main,
            })).collect::<Vec<_>>(),
            "branches": local,
            "remoteBranches": remote,
        })
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(message) => git_error(500, &message),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct WorktreeCreateBody {
    cwd: Option<String>,
    branch: Option<String>,
    action: Option<String>,
}

/// `POST /api/worktrees` —— `action: "checkout"` 是切分支，否则新建 worktree。
pub async fn worktrees_create(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<WorktreeCreateBody>,
) -> Response {
    state.sync_allowed_roots();
    let Some(cwd) = body.cwd.clone().filter(|s| !s.is_empty()) else {
        return git_error(400, "cwd is required");
    };
    let Some(branch) = body.branch.clone().filter(|s| !s.is_empty()) else {
        return git_error(400, "branch is required");
    };
    if !state.roots().allows(&cwd) || !state.roots().allows_real(&cwd) {
        return git_error(403, "Access denied");
    }
    if !Path::new(&cwd).exists() {
        return git_error(400, &format!("Directory does not exist: {cwd}"));
    }
    let checkout = body.action.as_deref() == Some("checkout");
    match blocking_git(move || {
        if checkout {
            gitsvc::checkout_branch(Path::new(&cwd), &branch)
                .map(|branch| json!({ "path": cwd, "branch": branch }))
        } else {
            gitsvc::create_worktree(Path::new(&cwd), &branch)
                .map(|(path, branch)| json!({ "path": path, "branch": branch }))
        }
    })
    .await
    {
        Ok(value) => Json(value).into_response(),
        Err(message) => git_error(400, &message),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct WorktreeDeleteBody {
    cwd: Option<String>,
    path: Option<String>,
    force: Option<bool>,
}

/// `DELETE /api/worktrees`
pub async fn worktrees_delete(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<WorktreeDeleteBody>,
) -> Response {
    state.sync_allowed_roots();
    let Some(cwd) = body.cwd.clone().filter(|s| !s.is_empty()) else {
        return git_error(400, "cwd is required");
    };
    let Some(path) = body.path.clone().filter(|s| !s.is_empty()) else {
        return git_error(400, "path is required");
    };
    if !state.roots().allows(&cwd) || !state.roots().allows_real(&cwd) {
        return git_error(403, "Access denied");
    }
    let force = body.force.unwrap_or(false);
    match blocking_git(move || gitsvc::remove_worktree(Path::new(&cwd), Path::new(&path), force))
        .await
    {
        Ok(()) => Json(json!({ "success": true })).into_response(),
        Err(message) => {
            let dirty = message.contains("contains modified or untracked files")
                || message.contains("is dirty");
            let mut response = git_error(if dirty { 409 } else { 400 }, &message);
            if dirty {
                let body = json!({ "error": message, "dirty": true });
                response = (StatusCode::CONFLICT, Json(body)).into_response();
            }
            response
        }
    }
}

// ---------------------------------------------------------------------------
// 阻塞桥
// ---------------------------------------------------------------------------

/// 把同步的 git 操作丢到阻塞线程池（失败=GitError → 转成消息）。
async fn blocking_git<F, T>(job: F) -> Result<T, String>
where
    F: FnOnce() -> Result<T, gitsvc::GitError> + Send + 'static,
    T: Send + 'static,
{
    match tokio::task::spawn_blocking(job).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(e)) => Err(e.to_string()),
        Err(e) => Err(e.to_string()),
    }
}

/// 不可失败的同步任务。
async fn blocking_plain<F, T>(job: F) -> Result<T, String>
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(job)
        .await
        .map_err(|e| e.to_string())
}
