//! 普通合并的三栏内容取自 Git 索引，而不是可能已变化的远程跟踪引用。

use super::{
    err, operation_in_progress, read_git_stdout, repo_lock, run_git, run_git_raw, GitError,
    ACTION_TIMEOUT, MAX_DIFF_FILE_BYTES, STATUS_TIMEOUT,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, File, Metadata, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

const LIMIT: usize = MAX_DIFF_FILE_BYTES as usize;

pub(super) fn paths(root: &Path) -> Result<Vec<String>, GitError> {
    let raw = run_git_raw(root, &["ls-files", "--unmerged", "-z"], STATUS_TIMEOUT)?;
    let mut paths = Vec::new();
    for (_, path) in entries(&raw) {
        if paths.last().map(String::as_str) != Some(path) {
            paths.push(path.to_owned());
        }
    }
    Ok(paths)
}

fn entries(raw: &str) -> impl Iterator<Item = (&str, &str)> {
    raw.split('\0').filter_map(|entry| entry.split_once('\t'))
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct ConflictFile {
    local: Option<String>,
    result: Option<String>,
    incoming: Option<String>,
    token: String,
    marker_size: usize,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum Resolution {
    Text { content: String },
    Delete,
}

struct Snapshot {
    data: ConflictFile,
    path: PathBuf,
    metadata: Option<Metadata>,
}

fn io_error(error: io::Error) -> GitError {
    err(format!("无法读写冲突文件：{error}"))
}

fn text(bytes: Vec<u8>) -> Result<String, GitError> {
    if bytes.len() > LIMIT || bytes.contains(&0) {
        return Err(err("冲突编辑器仅支持 256 KiB 以内的 UTF-8 文本"));
    }
    String::from_utf8(bytes).map_err(|_| err("冲突编辑器仅支持 UTF-8 文本"))
}

fn snapshot(root: &Path, path: &str) -> Result<Snapshot, GitError> {
    if operation_in_progress(root) != Some("merge") {
        return Err(err("当前没有正在进行的普通合并"));
    }
    if path.is_empty()
        || path.contains('\0')
        || path.split('/').any(|part| {
            part.is_empty() || part == "." || part == ".." || part.eq_ignore_ascii_case(".git")
        })
        || !Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        || (cfg!(windows) && path.contains(['\\', ':']))
    {
        return Err(err("无效的冲突文件路径"));
    }
    let raw = run_git_raw(
        root,
        &[
            "--literal-pathspecs",
            "ls-files",
            "--unmerged",
            "-z",
            "--",
            path,
        ],
        STATUS_TIMEOUT,
    )?;
    let mut sides = [None, None];
    let mut present = false;
    for (entry, _) in entries(&raw).filter(|(_, name)| *name == path) {
        let fields: Vec<_> = entry.split_whitespace().collect();
        let [mode, oid, stage] = fields.as_slice() else {
            return Err(err("无效的 Git 索引条目"));
        };
        present = true;
        if !matches!(*mode, "100644" | "100755") {
            return Err(err("请在终端处理符号链接或子模块冲突"));
        }
        if let Some(index) = match *stage {
            "2" => Some(0),
            "3" => Some(1),
            _ => None,
        } {
            let size = run_git(root, &["cat-file", "-s", oid], STATUS_TIMEOUT)?;
            if size.parse::<usize>().map_or(true, |size| size > LIMIT) {
                return Err(err("冲突编辑器仅支持 256 KiB 以内的 UTF-8 文本"));
            }
            let bytes = read_git_stdout(
                root,
                &["cat-file", "blob", oid],
                STATUS_TIMEOUT,
                &[],
                |out| {
                    let mut bytes = Vec::new();
                    out.take(LIMIT as u64 + 1)
                        .read_to_end(&mut bytes)
                        .map_err(io_error)?;
                    Ok(bytes)
                },
            )?;
            sides[index] = Some(text(bytes)?);
        }
    }
    if !present {
        return Err(err("该文件已不在冲突列表中"));
    }
    let target = root.join(path);
    // git add 与写文件都不能跨过被符号链接替换的目录或文件。
    let mut parent = target.parent().ok_or_else(|| err("无效的冲突文件路径"))?;
    while parent != root {
        let metadata = fs::symlink_metadata(parent).map_err(io_error)?;
        if !metadata.file_type().is_dir() || !super::is_inside(root, parent) {
            return Err(err("冲突文件的父目录不是仓库内的普通目录"));
        }
        parent = parent.parent().ok_or_else(|| err("无效的冲突文件路径"))?;
    }
    let (result, metadata) = match fs::symlink_metadata(&target) {
        Ok(metadata) if metadata.file_type().is_file() => {
            let mut bytes = Vec::new();
            File::open(&target)
                .map_err(io_error)?
                .take(LIMIT as u64 + 1)
                .read_to_end(&mut bytes)
                .map_err(io_error)?;
            (Some(text(bytes)?), Some(metadata))
        }
        Ok(_) => return Err(err("冲突路径不是普通文件")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => (None, None),
        Err(error) => return Err(io_error(error)),
    };
    let marker_size = run_git_raw(
        root,
        &["check-attr", "-z", "conflict-marker-size", "--", path],
        STATUS_TIMEOUT,
    )?
    .split('\0')
    .nth(2)
    .and_then(|value| value.parse::<usize>().ok())
    .filter(|size| *size > 0)
    .unwrap_or(7);
    let heads = run_git(root, &["rev-parse", "HEAD", "MERGE_HEAD"], STATUS_TIMEOUT)?;
    // 防止另一个窗口或终端修改工作区后，旧编辑器覆盖新的结果。
    let mut digest = Sha256::new();
    for part in [
        Some(root.to_string_lossy().as_ref()),
        Some(path),
        Some(&heads),
        Some(&raw),
        result.as_deref(),
    ] {
        digest.update((part.map_or(u64::MAX, |value| value.len() as u64)).to_le_bytes());
        if let Some(part) = part {
            digest.update(part.as_bytes());
        }
    }
    let [local, incoming] = sides;
    Ok(Snapshot {
        data: ConflictFile {
            local,
            result,
            incoming,
            token: format!("{:x}", digest.finalize()),
            marker_size,
        },
        path: target,
        metadata,
    })
}

pub(crate) fn read(root: &Path, path: &str) -> Result<ConflictFile, GitError> {
    let lock = repo_lock(root);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    Ok(snapshot(root, path)?.data)
}

fn has_markers(content: &str, size: usize) -> bool {
    content.lines().any(|line| {
        let Some(marker) = line.bytes().next().filter(|ch| b"<=>|".contains(ch)) else {
            return false;
        };
        let count = line.bytes().take_while(|ch| *ch == marker).count();
        count == size && (line.len() == count || line.as_bytes()[count] == b' ')
    })
}

fn replace_file(snapshot: &Snapshot, content: &str) -> Result<(), GitError> {
    let temp = snapshot
        .path
        .with_file_name(format!(".toiler-merge-{}", uuid::Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(io_error)?;
    let result = (|| {
        file.write_all(content.as_bytes())?;
        if let Some(metadata) = &snapshot.metadata {
            file.set_permissions(metadata.permissions())?;
        }
        drop(file);
        fs::rename(&temp, &snapshot.path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temp);
    }
    result.map_err(io_error)
}

pub(crate) fn resolve(
    root: &Path,
    path: &str,
    token: &str,
    resolution: Resolution,
) -> Result<(), GitError> {
    let lock = repo_lock(root);
    let _guard = lock.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let snapshot = snapshot(root, path)?;
    if snapshot.data.token != token {
        return Err(err("冲突文件已变化，请重新打开后再处理"));
    }
    match resolution {
        Resolution::Text { content } => {
            if content.len() > LIMIT || content.contains('\0') {
                return Err(err("合并结果必须是 256 KiB 以内的 UTF-8 文本"));
            }
            if has_markers(&content, snapshot.data.marker_size) {
                return Err(err("合并结果仍含 Git 冲突标记"));
            }
            if snapshot.data.result.as_deref() != Some(content.as_str()) {
                replace_file(&snapshot, &content)?;
            }
            run_git(
                root,
                &["--literal-pathspecs", "add", "--", path],
                ACTION_TIMEOUT,
            )?;
        }
        Resolution::Delete => {
            if snapshot.data.local.is_some() && snapshot.data.incoming.is_some() {
                return Err(err("仅允许确认冲突一方已删除的文件"));
            }
            run_git(
                root,
                &["--literal-pathspecs", "rm", "--", path],
                ACTION_TIMEOUT,
            )?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Repo(PathBuf);
    impl Drop for Repo {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn pull_conflicts_resolve_individually_and_continue_without_push() {
        let repo =
            Repo(std::env::temp_dir().join(format!("toiler-conflicts-{}", uuid::Uuid::new_v4())));
        fs::create_dir(&repo.0).unwrap();
        let git = |args: &[&str]| run_git(&repo.0, args, ACTION_TIMEOUT).unwrap();
        let file = "literal[1].txt";
        let deleted = "removed.txt";
        let put = |path, content: &str| fs::write(repo.0.join(path), content).unwrap();
        git(&["init", "-b", "main"]);
        git(&["config", "user.email", "test@example.invalid"]);
        git(&["config", "user.name", "合并测试"]);
        put(file, "base\n");
        put(deleted, "base\n");
        git(&["add", "-A"]);
        git(&["commit", "-m", "base"]);
        git(&["checkout", "-b", "incoming"]);
        put(file, "remote\n");
        git(&["rm", "--", deleted]);
        git(&["add", "-A"]);
        git(&["commit", "-m", "remote"]);
        git(&["checkout", "main"]);
        put(file, "local\n");
        put(deleted, "modified locally\n");
        git(&["add", "-A"]);
        git(&["commit", "-m", "local"]);
        assert!(run_git(
            &repo.0,
            &["pull", "--no-rebase", ".", "incoming"],
            ACTION_TIMEOUT
        )
        .is_err());
        assert_eq!(paths(&repo.0).unwrap(), [file, deleted]);
        let conflict = read(&repo.0, file).unwrap();
        assert_eq!(conflict.local.as_deref(), Some("local\n"));
        assert_eq!(conflict.incoming.as_deref(), Some("remote\n"));
        let result = conflict.result.unwrap();
        assert!(has_markers(&result, conflict.marker_size));
        assert!(resolve(
            &repo.0,
            file,
            &conflict.token,
            Resolution::Text { content: result }
        )
        .is_err());
        put(file, "external edit\n");
        assert!(resolve(
            &repo.0,
            file,
            &conflict.token,
            Resolution::Text {
                content: "stale\n".into()
            }
        )
        .is_err());
        let token = read(&repo.0, file).unwrap().token;
        resolve(
            &repo.0,
            file,
            &token,
            Resolution::Text {
                content: "local\nremote\n".into(),
            },
        )
        .unwrap();
        assert_eq!(paths(&repo.0).unwrap(), [deleted]);
        assert_eq!(git(&["show", &format!(":{file}")]), "local\nremote");
        assert!(read(&repo.0, file).is_err());
        let removed = read(&repo.0, deleted).unwrap();
        assert_eq!(removed.local.as_deref(), Some("modified locally\n"));
        assert!(removed.incoming.is_none());
        resolve(&repo.0, deleted, &removed.token, Resolution::Delete).unwrap();
        assert!(paths(&repo.0).unwrap().is_empty());
        assert!(!repo.0.join(deleted).exists());
        assert_eq!(operation_in_progress(&repo.0), Some("merge"));
        git(&["merge", "--continue"]);
        assert_eq!(operation_in_progress(&repo.0), None);
        assert_eq!(
            git(&["rev-list", "--parents", "-1", "HEAD"])
                .split_whitespace()
                .count(),
            3
        );
        assert!(git(&["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn marker_detection_respects_size_and_diff3() {
        assert!(has_markers("before\r\n||||||||| base\r\nafter", 9));
        assert!(!has_markers("=======\n<<<<<<<<<< other", 9));
        assert!(!has_markers("  <<<<<<< indented\n=======not-a-marker", 7));
    }
}
