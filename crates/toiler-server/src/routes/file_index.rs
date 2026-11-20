//! `@文件` 补全索引。只读允许根，不跟随目录符号链接，限制扫描深度与数量。

use std::collections::{BTreeSet, VecDeque};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, Instant};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::{security::Trusted, state::AppState};

#[derive(Deserialize)]
pub struct Params {
    cwd: Option<String>,
    q: Option<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Entry {
    path: String,
    is_dir: bool,
}

pub async fn get(
    State(state): State<AppState>,
    _trusted: Trusted,
    Query(params): Query<Params>,
) -> Response {
    state.sync_allowed_roots();
    let raw = params.cwd.as_deref().unwrap_or("").trim();
    if raw.is_empty() || !crate::paths::is_absolute_path(raw) {
        return error(StatusCode::BAD_REQUEST, "cwd 必须为绝对路径");
    }
    let cwd = PathBuf::from(raw);
    if !state.roots().allows(&cwd) || !state.roots().allows_real(&cwd) {
        return error(StatusCode::FORBIDDEN, "无权访问此目录");
    }
    if !cwd.is_dir() {
        return error(StatusCode::BAD_REQUEST, "cwd 不是可读取的目录");
    }
    let query: String = params.q.unwrap_or_default().chars().take(500).collect();
    // Git 会应用 .gitignore；非仓库或 git 不可用时退回有限扫描。
    // 没有 git 时连 spawn 都跳过，直接走扫描（它本来就会带上忽略规则）。
    let listed: Option<String> = if crate::gitsvc::git_available() {
        match tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::from(crate::gitsvc::git_command())
                .arg("-C")
                .arg(&cwd)
                .args([
                    "ls-files",
                    "--cached",
                    "--others",
                    "--exclude-standard",
                    "-z",
                ])
                .env("LC_ALL", "C")
                .kill_on_drop(true)
                .output(),
        )
        .await
        {
            Ok(Ok(output)) if output.status.success() => {
                Some(String::from_utf8_lossy(&output.stdout).to_string())
            }
            _ => None,
        }
    } else {
        None
    };
    let (files, truncated) = match listed {
        Some(stdout) => {
            let paths: BTreeSet<_> = stdout
                .split('\0')
                .filter(|v| !v.is_empty() && safe_relative(v))
                .take(200_001)
                .map(str::to_owned)
                .collect();
            let truncated = paths.len() > 200_000;
            (
                paths.into_iter().take(200_000).collect::<Vec<_>>(),
                truncated,
            )
        }
        None => match tokio::task::spawn_blocking(move || scan(&cwd)).await {
            Ok(listing) => listing,
            Err(_) => return error(StatusCode::INTERNAL_SERVER_ERROR, "文件索引扫描失败"),
        },
    };
    if query.is_empty() {
        let truncated = truncated || files.len() > 5_000;
        Json(json!({"files": files.into_iter().take(5_000).collect::<Vec<_>>(), "truncated": truncated})).into_response()
    } else {
        Json(json!({"matches": search(&files, &query)})).into_response()
    }
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({"error": message}))).into_response()
}

fn safe_relative(raw: &str) -> bool {
    !raw.contains(['\0', '\\'])
        && Path::new(raw)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn scan(root: &Path) -> (Vec<String>, bool) {
    let mut pending = VecDeque::from([(root.to_path_buf(), 0)]);
    let mut files = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut visited = 0;
    let mut truncated = false;
    while let Some((dir, depth)) = pending.pop_front() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            visited += 1;
            if files.len() >= 50_000 || visited >= 200_000 || Instant::now() >= deadline {
                files.sort();
                return (files, true);
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if [
                "node_modules",
                ".git",
                ".next",
                "dist",
                "build",
                "__pycache__",
                ".turbo",
                ".cache",
                "coverage",
                ".pytest_cache",
                ".mypy_cache",
                "target",
                "vendor",
                ".DS_Store",
            ]
            .contains(&name.as_ref())
                || name.ends_with(".pyc")
            {
                continue;
            }
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                if depth < 8 {
                    pending.push_back((entry.path(), depth + 1));
                } else {
                    truncated = true;
                }
            } else if kind.is_file() {
                if let Ok(relative) = entry.path().strip_prefix(root) {
                    files.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
    }
    files.sort();
    (files, truncated)
}

fn search(files: &[String], query: &str) -> Vec<Entry> {
    let mut dirs = BTreeSet::new();
    for file in files {
        for (index, _) in file.match_indices('/') {
            dirs.insert(file[..index].to_owned());
        }
    }
    let entries = dirs
        .into_iter()
        .map(|path| Entry { path, is_dir: true })
        .chain(files.iter().cloned().map(|path| Entry {
            path,
            is_dir: false,
        }));
    let query = query.to_lowercase();
    let mut scored: Vec<_> = entries
        .filter_map(|entry| {
            let path = entry.path.to_lowercase();
            let name = if query.contains('/') {
                path.as_str()
            } else {
                path.rsplit('/').next().unwrap_or(&path)
            };
            let score = if name == query {
                100
            } else if name.starts_with(&query) {
                80
            } else if name.contains(&query) {
                50
            } else if path.contains(&query) {
                30
            } else {
                let mut chars = path.chars();
                if query.chars().all(|needle| chars.any(|c| c == needle)) {
                    10
                } else {
                    0
                }
            };
            (score > 0).then_some((score + if entry.is_dir { 10 } else { 0 }, entry))
        })
        .collect();
    scored.sort_by(|(a_score, a), (b_score, b)| {
        b_score
            .cmp(a_score)
            .then_with(|| {
                a.path
                    .matches('/')
                    .count()
                    .cmp(&b.path.matches('/').count())
            })
            .then_with(|| a.path.cmp(&b.path))
    });
    scored
        .into_iter()
        .take(20)
        .map(|(_, entry)| entry)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranking_and_directory_drilldown() {
        let files = vec![
            "src/ChatInput.tsx".into(),
            "src/lib/chat.ts".into(),
            "README.md".into(),
        ];
        assert_eq!(search(&files, "chinp")[0].path, "src/ChatInput.tsx");
        assert!(search(&files, "src/")
            .iter()
            .all(|v| v.path.starts_with("src/")));
        for invalid in ["../secret", "/etc/passwd", "src/../../secret", "a\\b"] {
            assert!(!safe_relative(invalid));
        }
        assert!(safe_relative("src/你好.ts"));
    }
}
