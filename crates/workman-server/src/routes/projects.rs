//! `/api/projects`、`/api/project-trust`、`/api/cwd/*`。
//!
//! | route | 行为 |
//! |---|---|
//! | `GET    /api/projects` | 项目列表 + 上次选中的项目（顺带登记允许根） |
//! | `PUT    /api/projects` | 记住「选中的项目」 |
//! | `PATCH  /api/projects` | 改星标 |
//! | `DELETE /api/projects` | 移除项目 |
//! | `GET    /api/project-trust?cwd=` | `{requiresTrust, trusted}` |
//! | `POST   /api/project-trust` | 记录信任（没有需要信任的资源时 409） |
//! | `GET    /api/cwd/browse?path=` | 目录浏览器 |
//! | `POST   /api/cwd/validate` | 校验目录 + 登记项目 + 返回 `projectRoot` |
//!
//! 项目库见 [`crate::projects`]，信任见 [`crate::trust`]。

use std::path::{Path, PathBuf};

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};

use crate::paths;
use crate::security::Trusted;
use crate::state::AppState;
use crate::trust::{self, TrustStore};

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

fn no_store(mut response: Response) -> Response {
    response.headers_mut().insert(
        header::CACHE_CONTROL,
        header::HeaderValue::from_static("no-store"),
    );
    response
}

/// `GET /api/projects`
pub async fn list(State(state): State<AppState>, _trusted: Trusted) -> Response {
    match state.projects().list() {
        Ok(projects) => {
            // 与 pi 一致：把每个项目的根登记进允许根
            for project in &projects {
                state.roots().add(&project.project_root);
                state.roots().add(&project.cwd);
            }
            let body = json!({
                "projects": projects.iter().map(|p| p.to_json()).collect::<Vec<_>>(),
                "selectedProject": state.projects().selected_json(),
            });
            no_store(Json(body).into_response())
        }
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct SelectedBody {
    #[serde(rename = "selectedProject")]
    selected_project: Option<Value>,
}

/// `PUT /api/projects` —— 记住选中的项目。
pub async fn select(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<SelectedBody>,
) -> Response {
    let Some(selected) = body.selected_project else {
        return error(StatusCode::BAD_REQUEST, "selectedProject is invalid");
    };
    let key = selected
        .get("projectKey")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    let root = selected
        .get("projectRoot")
        .and_then(Value::as_str)
        .unwrap_or("")
        .trim()
        .to_string();
    if key.is_empty() || root.is_empty() {
        return error(StatusCode::BAD_REQUEST, "selectedProject is invalid");
    }
    if let Err(e) = state.projects().set_selected(&key, &root) {
        return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
    }
    Json(json!({ "success": true })).into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct StarBody {
    #[serde(rename = "projectKey")]
    project_key: Option<String>,
    starred: Option<bool>,
}

/// `PATCH /api/projects` —— 改星标。
pub async fn star(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<StarBody>,
) -> Response {
    let key = body.project_key.as_deref().unwrap_or("").trim().to_string();
    let Some(starred) = body.starred else {
        return error(
            StatusCode::BAD_REQUEST,
            "projectKey and starred are required",
        );
    };
    if key.is_empty() {
        return error(
            StatusCode::BAD_REQUEST,
            "projectKey and starred are required",
        );
    }
    match state.projects().set_starred(&key, starred) {
        Ok(true) => Json(json!({ "success": true, "starred": starred })).into_response(),
        Ok(false) => error(StatusCode::NOT_FOUND, "project not found"),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct RemoveBody {
    #[serde(rename = "projectKey")]
    project_key: Option<String>,
}

/// `DELETE /api/projects`
pub async fn remove(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<RemoveBody>,
) -> Response {
    let key = body.project_key.as_deref().unwrap_or("").trim().to_string();
    if key.is_empty() {
        return error(StatusCode::BAD_REQUEST, "projectKey is required");
    }
    match state.projects().remove(&key) {
        Ok(removed) => Json(json!({ "success": true, "removed": removed })).into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

#[derive(Debug, Deserialize, Default)]
pub struct CwdQuery {
    cwd: Option<String>,
}

/// `GET /api/project-trust?cwd=`
pub async fn trust_status(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(query): Query<CwdQuery>,
) -> Response {
    let Some(cwd) = query.cwd.as_deref().filter(|s| !s.trim().is_empty()) else {
        return error(StatusCode::BAD_REQUEST, "cwd required");
    };
    let resolved = crate::paths::absolute(&paths::expand_tilde(cwd.trim()));
    if !resolved.is_dir() {
        return error(StatusCode::BAD_REQUEST, "Directory does not exist");
    }
    if !allowed(&state, &resolved) {
        return error(StatusCode::FORBIDDEN, "Access denied");
    }
    let status = trust::status(state.project_trust(), &resolved.to_string_lossy());
    Json(json!({
        "requiresTrust": status.requires_trust,
        "trusted": status.trusted,
    }))
    .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct TrustBody {
    cwd: Option<String>,
}

/// `POST /api/project-trust` —— 记录信任。
pub async fn trust_set(
    State(state): State<AppState>,
    _trusted: Trusted,
    Json(body): Json<TrustBody>,
) -> Response {
    let Some(cwd) = body.cwd.as_deref().filter(|s| !s.trim().is_empty()) else {
        return error(StatusCode::BAD_REQUEST, "cwd required");
    };
    let resolved = crate::paths::absolute(&paths::expand_tilde(cwd.trim()));
    if !resolved.is_dir() {
        return error(StatusCode::BAD_REQUEST, "Directory does not exist");
    }
    if !allowed(&state, &resolved) {
        return error(StatusCode::FORBIDDEN, "Access denied");
    }
    let cwd_text = resolved.to_string_lossy().to_string();
    let status = trust::status(state.project_trust(), &cwd_text);
    if !status.requires_trust {
        return error(
            StatusCode::CONFLICT,
            "This project has no resources that require trust",
        );
    }
    // 有活 agent 在该目录跑着就拒绝（pi 的 `56429._I`）
    if state
        .live()
        .running_cwds()
        .iter()
        .any(|c| paths::absolute(Path::new(c)) == resolved)
    {
        return error(
            StatusCode::CONFLICT,
            "Wait for the active session to finish before trusting this project",
        );
    }
    match trust::mark_trusted(state.project_trust(), &cwd_text) {
        Ok(status) => Json(json!({
            "requiresTrust": status.requires_trust,
            "trusted": status.trusted,
        }))
        .into_response(),
        Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    }
}

fn allowed(state: &AppState, path: &Path) -> bool {
    state.sync_allowed_roots();
    state.roots().allows(path) || state.roots().allows_real(path)
}

#[derive(Debug, Deserialize, Default)]
pub struct BrowseQuery {
    path: Option<String>,
}

/// `GET /api/cwd/browse?path=` —— 目录浏览器。
pub async fn browse(_trusted: Trusted, Query(query): Query<BrowseQuery>) -> Response {
    let raw = query
        .path
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let target = match raw {
        None => paths::home_dir(),
        Some(text) => paths::expand_tilde(text),
    };
    // 与 pi 一致：realpath 之后再列（符号链接指到目录也认）
    let Ok(real) = std::fs::canonicalize(&target) else {
        return error(StatusCode::NOT_FOUND, "Directory does not exist");
    };
    if !real.is_dir() {
        return error(StatusCode::BAD_REQUEST, "Path is not a directory");
    }
    let mut directories: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&real) {
        for entry in entries.flatten() {
            let path = entry.path();
            let Ok(file_type) = entry.file_type() else {
                continue;
            };
            if file_type.is_dir() {
                directories.push(json!({
                    "name": entry.file_name().to_string_lossy(),
                    "path": path.to_string_lossy(),
                }));
                continue;
            }
            if !file_type.is_symlink() {
                continue;
            }
            let Ok(resolved) = std::fs::canonicalize(&path) else {
                continue;
            };
            if resolved.is_dir() {
                directories.push(json!({
                    "name": entry.file_name().to_string_lossy(),
                    "path": path.to_string_lossy(),
                }));
            }
        }
    }
    directories.sort_by(|a, b| {
        let an = a["name"].as_str().unwrap_or("");
        let bn = b["name"].as_str().unwrap_or("");
        an.to_lowercase().cmp(&bn.to_lowercase())
    });

    let parent = real.parent().map(|p| p.to_string_lossy().to_string());
    Json(json!({
        "path": real.to_string_lossy(),
        "parentPath": parent,
        "directories": directories,
    }))
    .into_response()
}

/// `POST /api/cwd/validate` —— 校验目录、登记允许根、把项目写进库。
///
/// pi 的语义（`cwd_validate.js`）：支持 `~` 与相对路径；不存在回 400；
/// 成功后返回 `{success, cwd, projectRoot, projectKey}`。
pub async fn validate(
    State(state): State<AppState>,
    _trusted: Trusted,
    body: Option<Json<Value>>,
) -> Response {
    let raw = body
        .as_ref()
        .and_then(|Json(value)| value.get("cwd"))
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("");
    if raw.is_empty() {
        return error(StatusCode::BAD_REQUEST, "Path is required");
    }
    let candidate = if raw == "~" || raw.starts_with("~/") {
        paths::expand_tilde(raw)
    } else if paths::is_absolute_path(raw) {
        PathBuf::from(raw)
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("/"))
            .join(raw)
    };
    let Ok(metadata) = std::fs::metadata(&candidate) else {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("Directory does not exist: {raw}"),
        );
    };
    if !metadata.is_dir() {
        return error(
            StatusCode::BAD_REQUEST,
            &format!("Path is not a directory: {raw}"),
        );
    }

    let cwd = candidate.to_string_lossy().to_string();
    state.roots().add(&cwd);
    let hint = crate::sessions_store::project_hint(&cwd);
    state.roots().add(&hint.root);
    let key = paths::project_key(&hint.root);
    let _ = state.projects().upsert(&key, &hint.root, &cwd);

    Json(json!({
        "success": true,
        "cwd": cwd,
        "projectRoot": hint.root,
        "projectKey": key,
    }))
    .into_response()
}

/// 供 `POST /api/default-cwd` 复用：建目录 + 登记允许根。
pub fn ensure_default_dir(state: &AppState, dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    state.roots().add(dir);
    Ok(())
}

/// 给测试用：把信任库换成临时目录里的。
#[allow(dead_code)]
pub fn trust_store_for(state: &AppState) -> &TrustStore {
    state.project_trust()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::AuthStore;
    use crate::models_json::ModelsFile;
    use crate::projects::ProjectStore;
    use crate::settings::SettingsFile;
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn app(dir: &Path) -> axum::Router {
        let catalog =
            crate::catalog::ModelCatalog::from_config(json!({"providers": {}}), None).unwrap();
        let state = AppState::with_cwd(dir, catalog, dir).with_stores(
            AuthStore::new(dir.join("auth.json")),
            ModelsFile::at(dir.join("models.json")),
            SettingsFile::at(dir.join("settings.json")),
            ProjectStore::at(dir.join("work.sqlite")),
            TrustStore::at(dir.join("trust.json")),
        );
        crate::routes::router(state)
    }

    async fn call(
        router: axum::Router,
        method: &str,
        uri: &str,
        body: Option<Value>,
    ) -> (StatusCode, Value) {
        let builder = Request::builder()
            .method(method)
            .uri(uri)
            .header("host", "127.0.0.1:8787");
        let request = match body {
            Some(value) => builder
                .header("content-type", "application/json")
                .body(Body::from(value.to_string()))
                .unwrap(),
            None => builder.body(Body::empty()).unwrap(),
        };
        let response = router.oneshot(request).await.unwrap();
        let status = response.status();
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )
    }

    #[tokio::test]
    async fn validate_registers_project_and_roundtrips() {
        let dir =
            std::env::temp_dir().join(format!("workman-route-projects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj")).unwrap();
        let router = app(&dir);
        let project = dir.join("proj");

        let (status, body) = call(
            router.clone(),
            "POST",
            "/api/cwd/validate",
            Some(json!({"cwd": project.to_string_lossy()})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["success"], true);
        assert_eq!(body["projectRoot"], project.to_string_lossy().as_ref());

        let (_, body) = call(router.clone(), "GET", "/api/projects", None).await;
        let projects = body["projects"].as_array().unwrap();
        assert_eq!(projects.len(), 1);
        assert_eq!(
            projects[0]["projectRoot"],
            project.to_string_lossy().as_ref()
        );

        let key = projects[0]["projectKey"].as_str().unwrap().to_string();
        let (status, _) = call(
            router.clone(),
            "PATCH",
            "/api/projects",
            Some(json!({"projectKey": key, "starred": true})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (status, _) = call(
            router.clone(),
            "PUT",
            "/api/projects",
            Some(json!({"selectedProject": {"projectKey": key, "projectRoot": project.to_string_lossy()}})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);

        let (_, body) = call(router.clone(), "GET", "/api/projects", None).await;
        assert_eq!(body["selectedProject"]["projectKey"], key);
        assert_eq!(body["projects"][0]["starred"], true);

        let (status, body) = call(
            router,
            "DELETE",
            "/api/projects",
            Some(json!({"projectKey": key})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["removed"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn trust_requires_resources() {
        let dir = std::env::temp_dir().join(format!("workman-route-trust-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("proj/.pi")).unwrap();
        let router = app(&dir);
        let project = dir.join("proj");
        let cwd = project.to_string_lossy().to_string();

        let (status, body) = call(
            router.clone(),
            "GET",
            &format!("/api/project-trust?cwd={cwd}"),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["requiresTrust"], false);
        assert_eq!(body["trusted"], true);

        // 写一个 .pi/settings.json → 需要信任
        std::fs::write(project.join(".pi/settings.json"), "{}").unwrap();
        let (_, body) = call(
            router.clone(),
            "GET",
            &format!("/api/project-trust?cwd={cwd}"),
            None,
        )
        .await;
        assert_eq!(body["requiresTrust"], true);
        assert_eq!(body["trusted"], false);

        let (status, body) = call(
            router.clone(),
            "POST",
            "/api/project-trust",
            Some(json!({"cwd": cwd})),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body["trusted"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn browse_lists_directories() {
        let dir = std::env::temp_dir().join(format!("workman-route-browse-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("alpha")).unwrap();
        std::fs::create_dir_all(dir.join("beta")).unwrap();
        std::fs::write(dir.join("file.txt"), "x").unwrap();
        let router = app(&dir);
        let (status, body) = call(
            router,
            "GET",
            &format!("/api/cwd/browse?path={}", dir.to_string_lossy()),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let names: Vec<&str> = body["directories"]
            .as_array()
            .unwrap()
            .iter()
            .map(|d| d["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, vec!["alpha", "beta"]);
        // 返回值是 realpath 之后的路径（macOS 上 /var → /private/var）
        assert_eq!(
            body["parentPath"],
            std::fs::canonicalize(dir.parent().unwrap())
                .unwrap()
                .to_string_lossy()
                .as_ref()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
