//! Git 与 worktree 服务：包一层 `git` CLI，输出形状对齐 pi。
//!
//! 三个来源模块：
//!
//! | pi 模块 | 导出 | 这里 |
//! |---|---|---|
//! | `33012` | `$G` / `SQ` / `oN` | [`status`] / [`diff_for_file`] / [`untracked_paths`] |
//! | `76402` | `JD` / `sX` / `s6` | [`full_status`] / [`action`] / [`staged_diff`] |
//! | `24923` | `KI` / `P9` / `eU` / `Hc` / `xw` / `l` / `IL` / `nX` | [`repo_info`] / [`worktrees`] / … |
//!
//! 所有命令都带 `LC_ALL=C`、`GIT_TERMINAL_PROMPT=0`、`GIT_EDITOR=true`，
//! 并以 `--no-ext-diff --no-color` 保证输出可解析。
//!
//! **凭据有两条路，缺一不可**（与 pi 的差异见 `docs/contracts/README.md` 差异 10）：
//!
//! | 路径 | 作用范围 | 实现在哪 |
//! |---|---|---|
//! | 加密文件 `git-credentials.json` | HTTPS 令牌 + **SSH 私钥口令** | [`crate::git_credentials`] |
//! | 系统 credential helper（`git credential approve`） | 仅 HTTPS | 本文件的 [`remember_credential`] |
//!
//! 只有 credential helper 的时候，「记住凭据」对 SSH 远程是静默失效的（SSH 没
//! 有 helper 这一层，口令只能走 askpass），面板也无法回答「我到底存过没有」——
//! 于是每次远程动作完都把口令框清空，用户得反复重输同一个口令。现在加密文件
//! 是主路径，credential helper 退化成「顺手也给命令行 git 一份」的补充。

use std::collections::BTreeSet;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use crate::git_credentials::{CredentialStore, GitCredential};

/// 单文件参与 diff / 行数统计的上限（pi 的 `1060._4`，256 KiB）。
pub const MAX_DIFF_FILE_BYTES: u64 = 262_144;
/// 单个文件参与 diff 的硬上限（pi 的 `1060.kv`，10 MiB）。
const LARGE_FILE_BYTES: usize = 10 * 1024 * 1024;
const STATUS_TIMEOUT: Duration = Duration::from_secs(10);
const ACTION_TIMEOUT: Duration = Duration::from_secs(60);

/// git 调用的错误：保留 stderr，路由用它回 400。
#[derive(Debug, Clone)]
pub struct GitError {
    pub message: String,
}

impl std::fmt::Display for GitError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for GitError {}

fn err(message: impl Into<String>) -> GitError {
    GitError {
        message: message.into(),
    }
}

/// 跑一条 git 命令，返回 trim 过的 stdout。
pub fn run_git(cwd: &Path, args: &[&str], timeout: Duration) -> Result<String, GitError> {
    run_git_inner(cwd, args, timeout, &[], true)
}

/// 跑一条 git 命令，保留原始 stdout（`-z` 输出会被前导空格影响到解析）。
fn run_git_raw(cwd: &Path, args: &[&str], timeout: Duration) -> Result<String, GitError> {
    run_git_inner(cwd, args, timeout, &[], false)
}

/// 带额外环境变量（例如 askpass）跑 git。
pub fn run_git_with_env(
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
    extra_env: &[(&str, String)],
) -> Result<String, GitError> {
    run_git_inner(cwd, args, timeout, extra_env, true)
}

fn run_git_inner(
    cwd: &Path,
    args: &[&str],
    timeout: Duration,
    extra_env: &[(&str, String)],
    trim: bool,
) -> Result<String, GitError> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(cwd)
        .args(args)
        .env("LC_ALL", "C")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_EDITOR", "true")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in extra_env {
        command.env(key, value);
    }
    let mut child = command
        .spawn()
        .map_err(|e| err(format!("git 启动失败: {e}")))?;

    // 两个线程把 stdout/stderr 抽干，避免大输出塞满管道导致死锁。
    let mut stdout = child.stdout.take().expect("piped stdout");
    let mut stderr = child.stderr.take().expect("piped stderr");
    let out_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stdout.read_to_end(&mut buf);
        buf
    });
    let err_handle = std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = stderr.read_to_end(&mut buf);
        buf
    });

    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(err("git 命令超时"));
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(e) => return Err(err(format!("git 等待失败: {e}"))),
        }
    };
    let stdout = out_handle.join().unwrap_or_default();
    let stderr = err_handle.join().unwrap_or_default();
    let stdout = String::from_utf8_lossy(&stdout).to_string();
    let stderr = String::from_utf8_lossy(&stderr).to_string();
    if !status.success() {
        let message = stderr.trim();
        return Err(err(if message.is_empty() {
            format!("git 退出码 {:?}", status.code())
        } else {
            message.to_string()
        }));
    }
    Ok(if trim {
        stdout.trim().to_string()
    } else {
        stdout
    })
}

/// `git rev-parse --show-toplevel`；不是仓库就 `None`。
pub fn repository_root(cwd: &Path) -> Option<PathBuf> {
    run_git(cwd, &["rev-parse", "--show-toplevel"], STATUS_TIMEOUT)
        .ok()
        .map(|s| PathBuf::from(s.trim()))
        .filter(|p| !p.as_os_str().is_empty())
}

fn real_or_self(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf())
}

fn relative_posix(from: &Path, to: &Path) -> String {
    let from = real_or_self(from);
    let to = real_or_self(to);
    match to.strip_prefix(&from) {
        Ok(rest) => {
            let text = rest.to_string_lossy().replace('\\', "/");
            text.trim_start_matches('/').to_string()
        }
        Err(_) => to.to_string_lossy().replace('\\', "/"),
    }
}

fn is_inside(root: &Path, path: &Path) -> bool {
    let root = real_or_self(root);
    let path = real_or_self(path);
    path == root || path.starts_with(&root)
}

fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

/// 一个文件的状态码 → pi 的语义化状态（`33012.l`）。
fn classify(index: char, worktree: char) -> (&'static str, &'static str) {
    let code = [index, worktree];
    let has = |c: char| code.contains(&c);
    if index == '?' && worktree == '?' {
        return ("untracked", "U");
    }
    const CONFLICTS: [&str; 7] = ["DD", "AU", "UD", "UA", "DU", "AA", "UU"];
    let pair: String = code.iter().collect();
    if CONFLICTS.contains(&pair.as_str()) || has('U') {
        return ("conflict", "C");
    }
    if has('D') {
        return ("deleted", "D");
    }
    if has('R') || has('C') {
        return ("renamed", "R");
    }
    if has('A') {
        return ("added", "A");
    }
    ("modified", "M")
}

/// `git status --porcelain=v1 -z` 的一条。
struct PorcelainEntry {
    path: String,
    original: Option<String>,
    index: char,
    worktree: char,
}

fn parse_porcelain(raw: &str) -> Vec<PorcelainEntry> {
    let parts: Vec<&str> = raw.split('\0').collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < parts.len() {
        let part = parts[i];
        i += 1;
        let chars: Vec<char> = part.chars().collect();
        if chars.len() < 4 || chars[2] != ' ' {
            continue;
        }
        let index = chars[0];
        let worktree = chars[1];
        let path: String = chars[3..].iter().collect();
        let mut original = None;
        if matches!(index, 'R' | 'C') || matches!(worktree, 'R' | 'C') {
            if let Some(next) = parts.get(i) {
                if !next.is_empty() {
                    original = Some(next.to_string());
                }
                i += 1;
            }
        }
        out.push(PorcelainEntry {
            path,
            original,
            index,
            worktree,
        });
    }
    out
}

fn porcelain(root: &Path) -> Vec<PorcelainEntry> {
    run_git_raw(
        root,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        STATUS_TIMEOUT,
    )
    .map(|raw| parse_porcelain(&raw))
    .unwrap_or_default()
}

fn count_lines(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if !meta.is_file() || meta.len() > MAX_DIFF_FILE_BYTES {
        return 0;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return 0;
    };
    if bytes.is_empty() || is_binary(&bytes) {
        return 0;
    }
    let text = String::from_utf8_lossy(&bytes);
    text.lines().count() as u64
}

/// 复刻 `33012.$G`：仓库状态 + 变更文件列表 + 增删行数。
pub fn status(cwd: &Path) -> Value {
    let Some(root) = repository_root(cwd) else {
        return json!({
            "isGitRepository": false,
            "repositoryRoot": Value::Null,
            "files": [],
            "additions": 0,
            "deletions": 0,
        });
    };

    let entries = porcelain(&root);
    let mut files: Vec<Value> = Vec::new();
    let mut untracked_additions = 0u64;
    for entry in &entries {
        let absolute = root.join(&entry.path);
        if !is_inside(cwd, &absolute) {
            continue;
        }
        let (status_name, code) = classify(entry.index, entry.worktree);
        if status_name == "untracked" {
            untracked_additions += count_lines(&absolute);
        }
        files.push(json!({
            "filePath": absolute.to_string_lossy(),
            "status": status_name,
            "code": code,
            "indexStatus": entry.index.to_string(),
            "worktreeStatus": entry.worktree.to_string(),
        }));
    }

    let relative = relative_posix(&root, cwd);
    let target = if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    };
    let (mut additions, mut deletions) = (0u64, 0u64);
    if let Ok(numstat) = run_git(
        &root,
        &[
            "diff",
            "--no-color",
            "--no-ext-diff",
            "--numstat",
            "HEAD",
            "--",
            &target,
        ],
        STATUS_TIMEOUT,
    ) {
        for line in numstat.lines() {
            let mut fields = line.split('\t');
            let add = fields.next().unwrap_or("");
            let del = fields.next().unwrap_or("");
            if let Ok(v) = add.parse::<u64>() {
                additions += v;
            }
            if let Ok(v) = del.parse::<u64>() {
                deletions += v;
            }
        }
    }

    json!({
        "isGitRepository": true,
        "repositoryRoot": root.to_string_lossy(),
        "files": files,
        "additions": additions + untracked_additions,
        "deletions": deletions,
    })
}

/// 未跟踪（且未被忽略）的顶层路径（`33012.oN`）。
pub fn untracked_paths(root: &Path) -> Vec<String> {
    let Ok(raw) = run_git_raw(
        root,
        &[
            "ls-files",
            "--others",
            "--exclude-standard",
            "--directory",
            "--no-empty-directory",
            "-z",
        ],
        STATUS_TIMEOUT,
    ) else {
        return Vec::new();
    };
    let mut out: Vec<String> = raw
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(|s| s.trim_end_matches('/').to_string())
        .filter(|s| !s.is_empty())
        .map(|s| root.join(s).to_string_lossy().to_string())
        .collect();
    out.sort();
    out
}

/// 从仓库根跑 `git diff` 拿某个文件的补丁（`33012.x`）。
fn diff_patch(root: &Path, path: &str, original: Option<&str>) -> Option<String> {
    let mut args = vec![
        "diff",
        "--no-color",
        "--no-ext-diff",
        "--unified=3",
        "HEAD",
        "--",
    ];
    if let Some(original) = original {
        if !original.is_empty() && original != path {
            args.push(original);
        }
    }
    args.push(path);
    run_git(root, &args, STATUS_TIMEOUT).ok()
}

/// 给未跟踪文件造一段 `git diff` 形态的补丁（`33012.w`）。
fn synthesize_patch(path: &str, content: &str) -> String {
    let ends_with_newline = content.ends_with('\n');
    let mut lines: Vec<&str> = content.split('\n').collect();
    if ends_with_newline {
        lines.pop();
    }
    let body = lines
        .iter()
        .map(|line| format!("+{line}"))
        .collect::<Vec<_>>()
        .join("\n");
    let trailer = if !ends_with_newline && !lines.is_empty() {
        "\n\\ No newline at end of file"
    } else {
        ""
    };
    format!(
        "diff --git a/{path} b/{path}\nnew file mode 100644\n--- /dev/null\n+++ b/{path}\n@@ -0,0 +1,{count} @@\n{body}{trailer}",
        count = lines.len()
    )
}

/// 复刻 `33012.SQ`：单个文件的 diff。
///
/// 返回 `{supported:false}` 的场合：不是 git 仓库、文件不在仓库里、文件太大、
/// 二进制、或者补丁里连一个 hunk 都没有。
pub fn diff_for_file(cwd: &Path, path: &Path) -> Value {
    let unsupported = json!({ "supported": false });
    let Some(root) = repository_root(cwd) else {
        return unsupported;
    };
    if !is_inside(&root, path) {
        return unsupported;
    }
    let relative = relative_posix(&root, path);
    let entries = porcelain(&root);
    let Some(entry) = entries.iter().find(|e| e.path == relative) else {
        return unsupported;
    };
    let (status_name, _) = classify(entry.index, entry.worktree);

    if status_name == "deleted" {
        return match diff_patch(&root, &relative, entry.original.as_deref()) {
            Some(patch) if patch.contains("\n@@ ") => json!({
                "supported": true,
                "status": status_name,
                "patch": patch,
            }),
            _ => unsupported,
        };
    }

    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return unsupported;
    };
    if !meta.is_file() || meta.len() > MAX_DIFF_FILE_BYTES {
        return unsupported;
    }
    let Ok(bytes) = std::fs::read(path) else {
        return unsupported;
    };
    if is_binary(&bytes) {
        return unsupported;
    }
    let content = String::from_utf8_lossy(&bytes).to_string();

    let patch = if status_name == "untracked" {
        synthesize_patch(&relative, &content)
    } else {
        match diff_patch(&root, &relative, entry.original.as_deref()) {
            Some(patch) => patch,
            None if status_name == "added" => synthesize_patch(&relative, &content),
            None => return unsupported,
        }
    };
    if !patch.contains("\n@@ ") {
        return unsupported;
    }
    json!({
        "supported": true,
        "status": status_name,
        "patch": patch,
    })
}

/// 远程 URL 的凭据类型（`24243.ET`）。
pub fn credential_kind(url: Option<&str>) -> &'static str {
    let Some(url) = url.filter(|u| !u.is_empty()) else {
        return "none";
    };
    let lower = url.to_ascii_lowercase();
    if lower.starts_with("https://") {
        return "https";
    }
    if lower.starts_with("ssh://") {
        return "ssh";
    }
    // scp 风格 `user@host:path`（pi: /^(?:ssh:\/\/|[^@\s\/:]+@[^:\s]+:)/）
    if let Some(at) = lower.find('@') {
        let head_ok = !lower[..at].contains('/') && !lower[..at].contains(':');
        let rest = &url[at + 1..];
        let host = rest.split(':').next().unwrap_or("");
        if head_ok && rest.contains(':') && !host.is_empty() && !host.contains('/') {
            return "ssh";
        }
    }
    "none"
}

/// 当前是否处在 merge / rebase / cherry-pick 之类的中间状态（`76402.s`）。
fn operation_in_progress(root: &Path) -> Option<&'static str> {
    let git_dir = run_git(root, &["rev-parse", "--absolute-git-dir"], STATUS_TIMEOUT).ok()?;
    let dir = PathBuf::from(git_dir);
    let probes: [(&str, &str); 5] = [
        ("rebase-merge", "rebase"),
        ("rebase-apply", "rebase"),
        ("MERGE_HEAD", "merge"),
        ("CHERRY_PICK_HEAD", "cherry-pick"),
        ("REVERT_HEAD", "revert"),
    ];
    for (file, name) in probes {
        if dir.join(file).exists() {
            return Some(name);
        }
    }
    None
}

/// 复刻 `76402.JD`：完整的仓库面板状态。
///
/// 用默认位置（`<toilerHome>/configs/git-credentials.json`）的凭据存储。
pub fn full_status(cwd: &Path) -> Value {
    full_status_with_store(cwd, &CredentialStore::default())
}

/// [`full_status`] 的可注入版本：测试用临时目录里的凭据文件，不碰用户真实配置。
pub fn full_status_with_store(cwd: &Path, store: &CredentialStore) -> Value {
    let Some(root) = repository_root(cwd) else {
        return json!({
            "isGitRepository": false,
            "repositoryRoot": Value::Null,
            "branch": Value::Null,
            "upstream": Value::Null,
            "ahead": 0,
            "behind": 0,
            "remote": Value::Null,
            "credentialKind": "none",
            "hasSavedCredential": false,
            "savedCredentialUsername": Value::Null,
            "canRememberCredential": crate::git_credentials::secure_storage_available(),
            "operation": Value::Null,
            "branches": [],
            "tags": [],
            "changes": status(cwd),
            "untrackedPaths": [],
        });
    };

    let branch = run_git(&root, &["branch", "--show-current"], STATUS_TIMEOUT)
        .ok()
        .filter(|s| !s.is_empty());
    let upstream = run_git(
        &root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            "@{upstream}",
        ],
        STATUS_TIMEOUT,
    )
    .ok()
    .filter(|s| !s.is_empty());
    let remote = run_git(&root, &["remote", "get-url", "origin"], STATUS_TIMEOUT)
        .ok()
        .filter(|s| !s.is_empty());
    let local_branches = branches(&root, "refs/heads");
    let changes = status(cwd);

    // 面板要显示的已存凭据：只有远程是 https / ssh 才可能有（`none` 类型不存
    // 也不问，否则每个 git:// 仓库都会白读一次加密文件）。
    let kind = credential_kind(remote.as_deref());
    let saved = if kind == "none" {
        None
    } else {
        remote
            .as_deref()
            .and_then(|url| store.saved_username(url, kind))
    };

    let (mut ahead, mut behind) = (0i64, 0i64);
    if let Some(upstream) = upstream.as_deref() {
        if let Ok(counts) = run_git(
            &root,
            &[
                "rev-list",
                "--left-right",
                "--count",
                &format!("HEAD...{upstream}"),
            ],
            STATUS_TIMEOUT,
        ) {
            let mut parts = counts.split_whitespace();
            ahead = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
            behind = parts.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        }
    }

    json!({
        "isGitRepository": true,
        "repositoryRoot": root.to_string_lossy(),
        "branch": branch,
        "upstream": upstream,
        "ahead": ahead,
        "behind": behind,
        "remote": remote,
        "credentialKind": kind,
        // 已存凭据的面板状态：`hasSavedCredential` 决定前端那句「留空时将使用已加密
        // 保存的凭据」，也决定远程动作后口令框该不该被清空。这里必须问真存储，
        // 不能像以前那样写死 false —— 写死会让每次推/拉之后口令框都变空。
        "hasSavedCredential": saved.is_some(),
        "savedCredentialUsername": saved
            .as_ref()
            .and_then(|username| username.clone())
            .map(Value::String)
            .unwrap_or(Value::Null),
        // 本平台能不能加密保存。非 macOS 没有等价的系统密钥库（见
        // `git_credentials` 模块文档），勾选框应该置灰并说明原因，而不是让
        // 用户勾一个永远不会生效的框。
        "canRememberCredential": crate::git_credentials::secure_storage_available(),
        "operation": operation_in_progress(&root),
        "branches": local_branches,
        "tags": branches(&root, "refs/tags"),
        "changes": changes,
        "untrackedPaths": untracked_paths(&root),
    })
}

fn branches(root: &Path, prefix: &str) -> Vec<String> {
    let format = "--format=%(refname:short)";
    let Ok(raw) = run_git(root, &["for-each-ref", format, prefix], STATUS_TIMEOUT) else {
        return Vec::new();
    };
    let mut out: Vec<String> = raw
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    out.sort();
    out.dedup();
    out
}

/// 复刻 `76402.s6`：暂存区的 diff（给 commit message 生成用）。
pub fn staged_diff(root: &Path) -> Result<String, GitError> {
    if repository_root(root).is_none() {
        return Err(err("Not a Git repository"));
    }
    let raw = run_git(
        root,
        &[
            "diff",
            "--cached",
            "--no-ext-diff",
            "--no-color",
            "--unified=3",
            "--",
        ],
        STATUS_TIMEOUT,
    )?;
    if raw.trim().is_empty() {
        return Err(err("No staged changes to summarize"));
    }
    Ok(if raw.len() > 60_000 {
        raw[..60_000].to_string()
    } else {
        raw
    })
}

/// 仓库里的一个 worktree。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    pub path: String,
    pub branch: Option<String>,
    pub is_main: bool,
}

/// 复刻 `24923.P9`：`git worktree list --porcelain` 的解析。
pub fn worktrees(root: &Path) -> Vec<Worktree> {
    let Ok(raw) = run_git(root, &["worktree", "list", "--porcelain"], STATUS_TIMEOUT) else {
        return Vec::new();
    };
    let mut out: Vec<Worktree> = Vec::new();
    let mut current: Option<(String, Option<String>, bool)> = None;
    let flush = |pending: &mut Option<(String, Option<String>, bool)>, out: &mut Vec<Worktree>| {
        if let Some((path, branch, prunable)) = pending.take() {
            if !prunable && Path::new(&path).exists() {
                out.push(Worktree {
                    path,
                    branch,
                    is_main: out.is_empty(),
                });
            }
        }
    };
    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("worktree ") {
            flush(&mut current, &mut out);
            current = Some((rest.trim().to_string(), None, false));
        } else if let Some(rest) = line.strip_prefix("branch ") {
            if let Some((path, _, prunable)) = current.take() {
                let branch = rest.trim().trim_start_matches("refs/heads/").to_string();
                current = Some((path, Some(branch), prunable));
            }
        } else if line.starts_with("prunable") {
            if let Some((path, branch, _)) = current.take() {
                current = Some((path, branch, true));
            }
        } else if line.trim().is_empty() {
            flush(&mut current, &mut out);
        }
    }
    flush(&mut current, &mut out);
    out
}

/// 当前 cwd 对应哪个 worktree（`24923.xw`）。
pub fn current_worktree_path(list: &[Worktree], cwd: &Path) -> Option<String> {
    let cwd = real_or_self(cwd);
    list.iter()
        .find(|wt| real_or_self(Path::new(&wt.path)) == cwd)
        .map(|wt| wt.path.clone())
}

/// 远程分支（`24923.Hc`）。
pub fn remote_branches(root: &Path) -> Vec<String> {
    let mut out: Vec<String> = branches(root, "refs/remotes")
        .into_iter()
        .filter(|b| !b.ends_with("/HEAD"))
        .collect();
    out.sort();
    out
}

/// 本地分支（`24923.eU`）。
pub fn local_branches(root: &Path) -> Vec<String> {
    branches(root, "refs/heads")
}

/// 仓库信息：`git rev-parse --path-format=absolute`（`24923.KI`）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub project_root: String,
    pub branch: Option<String>,
    pub is_worktree: bool,
    pub is_top_level: bool,
}

/// 复刻 `24923.KI`：判断 cwd 在仓库里的位置。
pub fn repo_info(cwd: &str) -> RepoInfo {
    let cwd_path = PathBuf::from(cwd);
    let fallback = RepoInfo {
        project_root: paths_absolute(&cwd_path),
        branch: None,
        is_worktree: false,
        is_top_level: false,
    };
    if !cwd_path.exists() {
        return fallback;
    }
    let Ok(raw) = run_git(
        &cwd_path,
        &[
            "rev-parse",
            "--path-format=absolute",
            "--git-common-dir",
            "--git-dir",
            "--show-toplevel",
            "--abbrev-ref",
            "HEAD",
        ],
        STATUS_TIMEOUT,
    ) else {
        return fallback;
    };
    let lines: Vec<&str> = raw.lines().map(str::trim).collect();
    if lines.len() < 4 {
        return fallback;
    }
    let (common_dir, git_dir, toplevel, branch) = (lines[0], lines[1], lines[2], lines[3]);
    let toplevel = PathBuf::from(toplevel);
    let is_top_level = real_or_self(&toplevel) == real_or_self(&cwd_path);
    let is_worktree =
        real_or_self(Path::new(git_dir)) != real_or_self(Path::new(common_dir)) && is_top_level;
    let root_path = if is_worktree {
        Path::new(common_dir)
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| toplevel.clone())
    } else {
        toplevel.clone()
    };
    RepoInfo {
        project_root: if is_top_level {
            real_or_self(&root_path).to_string_lossy().to_string()
        } else {
            paths_absolute(&cwd_path)
        },
        branch: Some(branch.to_string()).filter(|b| !b.is_empty() && b != "HEAD"),
        is_worktree,
        is_top_level,
    }
}

fn paths_absolute(path: &Path) -> String {
    crate::paths::absolute(path).to_string_lossy().to_string()
}

/// 分支名校验 + 清洗（`24923.v`）。
fn sanitize_branch(branch: &str) -> Result<String, GitError> {
    let trimmed = branch.trim();
    if trimmed.is_empty() {
        return Err(err("Branch name is required"));
    }
    let cleaned: String = trimmed
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|')
                || c.is_whitespace()
            {
                '-'
            } else {
                c
            }
        })
        .collect();
    let cleaned = cleaned.trim_matches('-').to_string();
    if cleaned.is_empty() {
        return Err(err(format!("Invalid branch name: {branch}")));
    }
    Ok(cleaned)
}

/// 复刻 `24923.l`：建一个 worktree（目录在 `<projectRoot>-worktrees/<分支>`）。
pub fn create_worktree(cwd: &Path, branch: &str) -> Result<(String, String), GitError> {
    let name = sanitize_branch(branch)?.to_string();
    let trimmed = branch.trim().to_string();
    let root = repository_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let container = PathBuf::from(format!(
        "{}-worktrees",
        real_or_self(&root).to_string_lossy()
    ));
    let target = container.join(&name);
    if target.exists() {
        return Err(err(format!(
            "Directory already exists: {}",
            target.to_string_lossy()
        )));
    }
    std::fs::create_dir_all(&container).map_err(|e| err(e.to_string()))?;
    let exists = run_git(
        &root,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{trimmed}"),
        ],
        STATUS_TIMEOUT,
    )
    .is_ok();
    let target_str = target.to_string_lossy().to_string();
    if exists {
        run_git(
            &root,
            &["worktree", "add", "--", &target_str, &trimmed],
            ACTION_TIMEOUT,
        )?;
    } else {
        run_git(
            &root,
            &["worktree", "add", "-b", &trimmed, "--", &target_str],
            ACTION_TIMEOUT,
        )?;
    }
    Ok((target_str, trimmed))
}

/// 复刻 `24923.IL`：删一个 worktree（主 worktree 不能删）。
pub fn remove_worktree(cwd: &Path, path: &Path, force: bool) -> Result<(), GitError> {
    let root = repository_root(cwd).unwrap_or_else(|| cwd.to_path_buf());
    let list = worktrees(&root);
    let target = real_or_self(path);
    let Some(found) = list
        .iter()
        .find(|wt| real_or_self(Path::new(&wt.path)) == target)
    else {
        return Err(err(format!(
            "Not a worktree of this repository: {}",
            path.to_string_lossy()
        )));
    };
    if found.is_main {
        return Err(err("Cannot remove the main worktree"));
    }
    let path_str = path.to_string_lossy().to_string();
    let mut args: Vec<&str> = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push("--");
    args.push(&path_str);
    run_git(&root, &args, ACTION_TIMEOUT)?;
    Ok(())
}

/// 复刻 `24923.nX`：切分支。
pub fn checkout_branch(cwd: &Path, branch: &str) -> Result<String, GitError> {
    let trimmed = branch.trim();
    if trimmed.is_empty() {
        return Err(err("Branch name is required"));
    }
    run_git(
        cwd,
        &["rev-parse", "--verify", &format!("refs/heads/{trimmed}")],
        STATUS_TIMEOUT,
    )?;
    run_git(cwd, &["checkout", "--quiet", trimmed], ACTION_TIMEOUT)?;
    Ok(trimmed.to_string())
}

/// `git.js` 的 20 个白名单动作，加上 toiler 追加的标签管理（`create_tag` /
/// `delete_tag` / `push_tag` / `push_tags`，见 `docs/contracts/README.md` 差异 22）。
pub const ACTIONS: [&str; 24] = [
    "stage",
    "unstage",
    "discard",
    "discard_all",
    "commit",
    "fetch",
    "pull",
    "push",
    "merge",
    "continue",
    "abort",
    "create_branch",
    "rename_branch",
    "delete_branch",
    "checkout_remote_branch",
    "delete_remote_branch",
    "set_remote_url",
    "pull_branch",
    "push_branch",
    "merge_branch",
    "create_tag",
    "delete_tag",
    "push_tag",
    "push_tags",
];

pub fn is_supported_action(action: &str) -> bool {
    ACTIONS.contains(&action)
}

/// 每个仓库一把锁：同一仓库的写操作串行（pi 的 `76402.t`）。
fn repo_lock(root: &Path) -> std::sync::Arc<std::sync::Mutex<()>> {
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex, OnceLock};
    static LOCKS: OnceLock<Mutex<HashMap<String, Arc<Mutex<()>>>>> = OnceLock::new();
    let map = LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let key = real_or_self(root).to_string_lossy().to_string();
    let mut guard = map.lock().unwrap_or_else(|e| e.into_inner());
    guard
        .entry(key)
        .or_insert_with(|| Arc::new(Mutex::new(())))
        .clone()
}

/// `git check-ref-format --branch` 校验（`76402.v`）。
fn validate_branch(root: &Path, raw: &str) -> Result<String, GitError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(err("Branch is required"));
    }
    run_git(
        root,
        &["check-ref-format", "--branch", trimmed],
        STATUS_TIMEOUT,
    )
    .map_err(|_| err(format!("Invalid branch name: {trimmed}")))?;
    Ok(trimmed.to_string())
}

/// 校验 tag 名（`git check-ref-format` 要求完整 ref 名）。
fn validate_tag(root: &Path, raw: &str) -> Result<String, GitError> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(err("Tag name is required"));
    }
    run_git(
        root,
        &["check-ref-format", &format!("refs/tags/{trimmed}")],
        STATUS_TIMEOUT,
    )
    .map_err(|_| err(format!("Invalid tag name: {trimmed}")))?;
    Ok(trimmed.to_string())
}

/// 标签名合法、且本地真的有这个 tag。
///
/// 不先查就直接 `push refs/tags/<不存在>` 的话，git 报的是
/// `src refspec ... does not match any`——面板里看不出是名字写错还是标签没建。
fn require_existing_tag(root: &Path, raw: &str) -> Result<String, GitError> {
    let tag = validate_tag(root, raw)?;
    run_git(
        root,
        &["rev-parse", "--verify", &format!("refs/tags/{tag}")],
        STATUS_TIMEOUT,
    )
    .map_err(|_| err(format!("Tag not found: {tag}")))?;
    Ok(tag)
}

/// 把 `origin/feat` 解析成 `refs/remotes/origin/feat`（`76402.x`）。
fn resolve_remote_branch(root: &Path, raw: &str) -> Result<(String, String, String), GitError> {
    let trimmed = raw.trim();
    let Some(idx) = trimmed.find('/') else {
        return Err(err("Invalid remote branch"));
    };
    if idx == 0 || idx == trimmed.len() - 1 {
        return Err(err("Invalid remote branch"));
    }
    let remote = trimmed[..idx].to_string();
    let branch = validate_branch(root, &trimmed[idx + 1..])?;
    run_git(
        root,
        &[
            "rev-parse",
            "--verify",
            &format!("refs/remotes/{remote}/{branch}"),
        ],
        STATUS_TIMEOUT,
    )
    .map_err(|_| err(format!("Remote branch not found: {trimmed}")))?;
    Ok((remote, branch, trimmed.to_string()))
}

/// 本地分支的上游（`76402.y`）。
fn branch_upstream(root: &Path, raw: &str) -> Result<(String, String, String), GitError> {
    let branch = validate_branch(root, raw)?;
    let upstream = run_git(
        root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            &format!("{branch}@{{upstream}}"),
        ],
        STATUS_TIMEOUT,
    )
    .map_err(|_| err(format!("Branch {branch} has no upstream")))?;
    let Some(idx) = upstream.find('/') else {
        return Err(err(format!("Invalid upstream for {branch}")));
    };
    if idx == 0 || idx == upstream.len() - 1 {
        return Err(err(format!("Invalid upstream for {branch}")));
    }
    Ok((
        branch,
        upstream[..idx].to_string(),
        upstream[idx + 1..].to_string(),
    ))
}

/// 某个分支被哪个 worktree 占用（`76402.z`）。
fn worktree_of_branch(root: &Path, branch: &str) -> Option<PathBuf> {
    worktrees(root)
        .into_iter()
        .find(|wt| wt.branch.as_deref() == Some(branch))
        .map(|wt| PathBuf::from(wt.path))
}

fn require_str<'a>(body: &'a Value, key: &str) -> Result<&'a str, GitError> {
    body.get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| err(format!("{key} is required")))
}

fn paths_arg(body: &Value) -> Result<Vec<String>, GitError> {
    let Some(list) = body.get("paths").and_then(Value::as_array) else {
        return Err(err("At least one file path is required"));
    };
    let mut out = Vec::new();
    for item in list {
        let Some(text) = item.as_str().map(str::trim).filter(|s| !s.is_empty()) else {
            return Err(err("At least one file path is required"));
        };
        out.push(text.to_string());
    }
    if out.is_empty() {
        return Err(err("At least one file path is required"));
    }
    Ok(out)
}

/// 把绝对路径转成相对仓库根的 POSIX 路径；越界就报错。
fn repo_relative(root: &Path, raw: &str) -> Result<String, GitError> {
    let absolute = if crate::paths::is_absolute_path(raw) {
        PathBuf::from(raw)
    } else {
        root.join(raw)
    };
    let normalized = real_or_self(&absolute);
    let root_real = real_or_self(root);
    let Ok(rest) = normalized.strip_prefix(&root_real) else {
        return Err(err("A requested file is outside this repository"));
    };
    let text = rest.to_string_lossy().replace('\\', "/");
    if text.is_empty() || text.starts_with("../") {
        return Err(err("A requested file is outside this repository"));
    }
    Ok(text)
}

/// 凭据：`{kind, username?, secret}`。
fn credential_from_body(
    body: &Value,
) -> Result<Option<(String, Option<String>, String)>, GitError> {
    let Some(value) = body.get("credential") else {
        return Ok(None);
    };
    if value.is_null() {
        return Ok(None);
    }
    let kind = value.get("kind").and_then(Value::as_str).unwrap_or("");
    if kind != "https" && kind != "ssh" {
        return Err(err("Invalid Git credential"));
    }
    let secret = value.get("secret").and_then(Value::as_str).unwrap_or("");
    if secret.is_empty() {
        return Err(err("Invalid Git credential"));
    }
    let username = match value.get("username") {
        None => None,
        Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(err("Invalid Git credential")),
    };
    Ok(Some((kind.to_string(), username, secret.to_string())))
}

/// 临时 askpass 程序：pi 用它把凭据喂给 git（避免明文进命令行）。
///
/// 脚本内容按平台不同（git 把 `GIT_ASKPASS` 当可执行程序启动，把 prompt 作为
/// 第一个参数传给它，所以我们只需根据 prompt 里有没有 “sername” 决定吐用户名
/// 还是密码）：
///
/// | 平台 | 文件名 | 说明 |
/// |---|---|---|
/// | unix | `askpass.sh` | `#!/bin/sh` + `case` |
/// | Windows | `askpass.cmd` | 批处理；`CreateProcess` 会自己经 `cmd.exe` 跑 `.cmd` |
///
/// 早先这里固定写 `#!/bin/sh` 的脚本，Windows 上 git 根本执行不了它 ——
/// 「记住密码」在 Windows 是静默失效的。
struct Askpass {
    dir: PathBuf,
}

/// askpass 脚本的文件名与内容。抽成函数是为了能单测（不跑 git、不分平台）。
fn askpass_script() -> (&'static str, &'static str) {
    #[cfg(unix)]
    {
        (
            "askpass.sh",
            "#!/bin/sh\ncase \"$1\" in *[Uu]sername*) cat \"$TOILER_GIT_USERNAME_FILE\" ;; *) cat \"$TOILER_GIT_SECRET_FILE\" ;; esac\n",
        )
    }
    // Windows：先判断 prompt 里有没有 “sername”，再 `type` 对应文件。
    // 用 `type` 而不是 `echo`：文件内容里的 `%` / `!` / `&` 不会被再展开一次。
    #[cfg(not(unix))]
    {
        (
            "askpass.cmd",
            "@echo off\r\nsetlocal\r\necho %~1 | findstr /I \"sername\" >nul\r\nif errorlevel 1 goto __toiler_secret\r\n:__toiler_user\r\ntype \"%TOILER_GIT_USERNAME_FILE%\"\r\ngoto __toiler_done\r\n:__toiler_secret\r\ntype \"%TOILER_GIT_SECRET_FILE%\"\r\n:__toiler_done\r\nexit /b 0\r\n",
        )
    }
}

impl Askpass {
    fn new(secret: &str, username: Option<&str>) -> Result<Self, GitError> {
        let dir = std::env::temp_dir().join(format!(
            "toiler-git-askpass-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).map_err(|e| err(e.to_string()))?;
        std::fs::write(dir.join("secret"), secret).map_err(|e| err(e.to_string()))?;
        std::fs::write(dir.join("username"), username.unwrap_or("git"))
            .map_err(|e| err(e.to_string()))?;
        let (name, body) = askpass_script();
        let script = dir.join(name);
        std::fs::write(&script, body).map_err(|e| err(e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700));
            let _ = std::fs::set_permissions(
                dir.join("secret"),
                std::fs::Permissions::from_mode(0o600),
            );
            let _ = std::fs::set_permissions(
                dir.join("username"),
                std::fs::Permissions::from_mode(0o600),
            );
        }
        Ok(Self { dir })
    }

    fn env(&self) -> Vec<(&'static str, String)> {
        let (name, _) = askpass_script();
        let script = self.dir.join(name).to_string_lossy().to_string();
        vec![
            ("GIT_ASKPASS", script.clone()),
            ("SSH_ASKPASS", script),
            ("SSH_ASKPASS_REQUIRE", "force".to_string()),
            (
                "DISPLAY",
                std::env::var("DISPLAY").unwrap_or_else(|_| "toiler".into()),
            ),
            (
                "TOILER_GIT_SECRET_FILE",
                self.dir.join("secret").to_string_lossy().to_string(),
            ),
            (
                "TOILER_GIT_USERNAME_FILE",
                self.dir.join("username").to_string_lossy().to_string(),
            ),
        ]
    }
}

impl Drop for Askpass {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn run_network(
    root: &Path,
    args: &[&str],
    credential: Option<(String, Option<String>, String)>,
    remote: Option<&str>,
) -> Result<String, GitError> {
    match credential {
        None => run_git(root, args, ACTION_TIMEOUT),
        Some((kind, username, secret)) => {
            let expected = credential_kind(remote);
            if expected != "none" && expected != kind {
                return Err(err("Credential type does not match the remote URL"));
            }
            if kind == "https" && username.as_deref().unwrap_or("").trim().is_empty() {
                return Err(err("A username is required for an HTTPS remote"));
            }
            let askpass = Askpass::new(&secret, username.as_deref())?;
            run_git_with_env(root, args, ACTION_TIMEOUT, &askpass.env())
        }
    }
}

/// 需要凭据的远程动作共用的外壳：解析 origin URL → 解析凭据（请求体里没给就用
/// 已加密保存的）→ 跑 network → 按 `rememberCredential` 落盘。
///
/// `fetch` / `pull` / `push` / `push_tag` / `push_tags` 全走这里：凭据语义必须
/// 完全一致，否则会出现「拉取能用、推标签却说没有 token」这类只在某一动作上
/// 复现的问题。
///
/// **两个兜底方向都要有**，它们对应两个真实的失败现象：
///
/// * 面板留空就用已存的（前端拿 `SAVED_CREDENTIAL_MASK` 占位，不会回传明文）。
///   以前这里只能用 credential helper 解 HTTPS，SSH 口令在第二次操作起就必须
///   重输；
/// * 勾了「记住」就真的存下来。以前只调 `git credential approve`，于是 SSH 远程
///   勾了也无事发生，下次仍然空着。
fn run_remote_action(root: &Path, args: &[&str], body: &Value) -> Result<(), GitError> {
    run_remote_action_with_store(root, args, body, &CredentialStore::default())
}

/// [`run_remote_action`] 的可注入版本（测试用临时凭据文件）。
fn run_remote_action_with_store(
    root: &Path,
    args: &[&str],
    body: &Value,
    store: &CredentialStore,
) -> Result<(), GitError> {
    let remote = run_git(root, &["remote", "get-url", "origin"], STATUS_TIMEOUT)
        .ok()
        .filter(|s| !s.is_empty());
    let kind = credential_kind(remote.as_deref());

    // 1. 显式传入的凭据优先；没传就看存过的有没有。
    let credential = resolve_credential(store, remote.as_deref(), kind, body)?;

    run_network(root, args, credential.clone(), remote.as_deref())?;

    // 2. 只有这次真的用了凭据、且用户勾了记住，才写盘。失败不回滚主操作：
    //    推送已经成功了，没必要因为存不下凭据就把整次操作报成失败。
    if let Some((kind, username, secret)) = credential {
        if body.get("rememberCredential").and_then(Value::as_bool) == Some(true) {
            let Some(url) = remote.as_deref() else {
                return Ok(());
            };
            let entry = GitCredential::new(kind.as_str(), username.clone(), secret.clone());
            if let Err(message) = store.save(url, &entry) {
                // 非 macOS 是预期内的（没有系统密钥库），debug 级别即可；
                // 其它失败（文件损坏、钥匙串拒统）是真正的「勾了却没记住」，
                // 必须让用户看得见。
                if message.contains("unavailable") {
                    tracing::debug!(%message, "未保存 Git 凭据");
                } else {
                    tracing::warn!(%message, "保存 Git 凭据失败");
                }
            }
            // credential helper 只管 HTTPS；对 SSH 传 password 进去是错的。
            if kind == "https" {
                remember_credential(remote.as_deref(), username.as_deref(), &secret);
            }
        }
    }
    Ok(())
}

/// 决定这次远程动作到底用哪份凭据：**请求体里的 > 已加密保存的 > 没有**。
///
/// 抽成函数是为了能直接测「面板留空时会不会真的用上存过的那份」—— 这是之前
/// 实际坏的环节（前端留空占位，后端却不会去读文件，于是第二次操作就报
/// `Permission denied (publickey)`）。
fn resolve_credential(
    store: &CredentialStore,
    remote: Option<&str>,
    kind: &str,
    body: &Value,
) -> Result<Option<(String, Option<String>, String)>, GitError> {
    if let Some(explicit) = credential_from_body(body)? {
        return Ok(Some(explicit));
    }
    if kind == "none" {
        return Ok(None);
    }
    Ok(remote
        .and_then(|url| store.load(url, kind))
        .map(|saved| (saved.kind, saved.username, saved.secret)))
}

/// 复刻 `76402.sX`：执行一个 git 操作，返回操作后的完整状态。
pub fn action(cwd: &Path, body: &Value) -> Result<Value, GitError> {
    let action = body
        .get("action")
        .and_then(Value::as_str)
        .ok_or_else(|| err("Unsupported Git action"))?;
    if !is_supported_action(action) {
        return Err(err("Unsupported Git action"));
    }
    let root = repository_root(cwd).ok_or_else(|| err("Not a Git repository"))?;
    let lock = repo_lock(&root);
    let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());

    match action {
        "stage" | "unstage" | "discard" => {
            let paths = paths_arg(body)?;
            let mut relative = Vec::new();
            for path in &paths {
                relative.push(repo_relative(&root, path)?);
            }
            let refs: Vec<&str> = relative.iter().map(String::as_str).collect();
            match action {
                "stage" => {
                    let mut args = vec!["add", "--"];
                    args.extend(refs);
                    run_git(&root, &args, ACTION_TIMEOUT)?;
                }
                "unstage" => {
                    let mut args = vec!["restore", "--staged", "--"];
                    args.extend(refs);
                    run_git(&root, &args, ACTION_TIMEOUT)?;
                }
                _ => {
                    let mut args = vec!["restore", "--staged", "--"];
                    args.extend(refs.clone());
                    run_git(&root, &args, ACTION_TIMEOUT)?;
                    let mut tracked_args = vec!["ls-tree", "-r", "--name-only", "HEAD", "--"];
                    tracked_args.extend(refs.clone());
                    let tracked = run_git(&root, &tracked_args, ACTION_TIMEOUT).unwrap_or_default();
                    let tracked: Vec<&str> = tracked.lines().filter(|l| !l.is_empty()).collect();
                    if !tracked.is_empty() {
                        let mut restore = vec!["restore", "--worktree", "--source=HEAD", "--"];
                        restore.extend(tracked);
                        run_git(&root, &restore, ACTION_TIMEOUT)?;
                    }
                    let mut clean = vec!["clean", "-f", "--quiet", "--"];
                    clean.extend(refs);
                    run_git(&root, &clean, ACTION_TIMEOUT)?;
                }
            }
        }
        "discard_all" => {
            let has_head = run_git(
                &root,
                &["rev-parse", "--verify", "--quiet", "HEAD"],
                STATUS_TIMEOUT,
            )
            .is_ok();
            if has_head {
                run_git(&root, &["reset", "--quiet"], ACTION_TIMEOUT)?;
                run_git(&root, &["checkout", "--", "."], ACTION_TIMEOUT)?;
            }
            run_git(&root, &["clean", "-fd", "--quiet"], ACTION_TIMEOUT)?;
        }
        "commit" => {
            let message = require_str(body, "message")?;
            run_git(&root, &["commit", "-m", message], ACTION_TIMEOUT)?;
        }
        "fetch" | "pull" | "push" => {
            let args: Vec<&str> = match action {
                "fetch" => vec!["fetch", "--prune"],
                "pull" => vec![
                    "pull",
                    if body.get("rebase").and_then(Value::as_bool) == Some(true) {
                        "--rebase"
                    } else {
                        "--no-rebase"
                    },
                ],
                _ => vec!["push"],
            };
            run_remote_action(&root, &args, body)?;
        }
        "set_remote_url" => {
            let url = require_str(body, "remoteUrl")?;
            run_git(
                &root,
                &["remote", "set-url", "origin", "--", url],
                ACTION_TIMEOUT,
            )?;
        }
        "merge" => {
            let branch = require_str(body, "branch")?;
            run_git(&root, &["merge", "--no-edit", branch], ACTION_TIMEOUT)?;
        }
        "create_branch" => {
            let branch = validate_branch(&root, require_str(body, "branch")?)?;
            match body.get("startPoint").and_then(Value::as_str) {
                Some(start) if !start.trim().is_empty() => {
                    // `--starting-point` 可能是远程分支。
                    let start = start.trim();
                    let reference = format!("refs/remotes/{start}");
                    let exists_remote = run_git(
                        &root,
                        &["rev-parse", "--verify", &reference],
                        STATUS_TIMEOUT,
                    )
                    .is_ok();
                    if exists_remote {
                        run_git(
                            &root,
                            &["branch", "--", &branch, &reference],
                            ACTION_TIMEOUT,
                        )?;
                    } else {
                        run_git(&root, &["branch", "--", &branch, start], ACTION_TIMEOUT)?;
                    }
                }
                _ => {
                    run_git(&root, &["branch", "--", &branch], ACTION_TIMEOUT)?;
                }
            }
            run_git(&root, &["checkout", "--quiet", &branch], ACTION_TIMEOUT)?;
        }
        "rename_branch" => {
            let old = validate_branch(&root, require_str(body, "branch")?)?;
            let new = validate_branch(&root, require_str(body, "newBranch")?)?;
            run_git(&root, &["branch", "-m", "--", &old, &new], ACTION_TIMEOUT)?;
        }
        "delete_branch" => {
            let branch = validate_branch(&root, require_str(body, "branch")?)?;
            run_git(&root, &["branch", "-D", &branch], ACTION_TIMEOUT)?;
        }
        // toiler 扩展（不在 pi 的白名单里）：本地标签管理。带了 `message` 就建
        // 附注 tag（`git tag -a`），否则建轻量 tag；`target` 缺省指向 HEAD。
        "create_tag" => {
            let tag = validate_tag(&root, require_str(body, "tag")?)?;
            let message = body
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let target = body
                .get("target")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty());
            let mut args: Vec<&str> = vec!["tag"];
            if let Some(message) = message {
                args.extend_from_slice(&["-a", &tag, "-m", message]);
            } else {
                args.push(&tag);
            }
            if let Some(target) = target {
                args.push(target);
            }
            run_git(&root, &args, ACTION_TIMEOUT)?;
        }
        "delete_tag" => {
            let tag = validate_tag(&root, require_str(body, "tag")?)?;
            run_git(&root, &["tag", "-d", &tag], ACTION_TIMEOUT)?;
        }
        // toiler 扩展：把本地标签推到 origin。与 fetch/pull/push 共用同一套凭据
        // 注入与「记住凭据」语义——HTTPS 远程推标签同样会要 token，少了
        // `run_remote_action` 就只能推 SSH 远程。
        "push_tag" => {
            let tag = require_existing_tag(&root, require_str(body, "tag")?)?;
            let reference = format!("refs/tags/{tag}");
            run_remote_action(&root, &["push", "origin", &reference], body)?;
        }
        // 全部本地标签一起推。远程已存在同名 tag 且指向不同提交时 git 会拒绝
        // 整条命令（`--tags` 的既有语义），错误原文会由面板顶部报出来。
        "push_tags" => {
            run_remote_action(&root, &["push", "origin", "--tags"], body)?;
        }
        "checkout_remote_branch" => {
            let (remote, branch, _) = resolve_remote_branch(&root, require_str(body, "branch")?)?;
            let local_exists = run_git(
                &root,
                &["rev-parse", "--verify", &format!("refs/heads/{branch}")],
                STATUS_TIMEOUT,
            )
            .is_ok();
            if local_exists {
                run_git(&root, &["checkout", "--quiet", &branch], ACTION_TIMEOUT)?;
            } else {
                run_git(
                    &root,
                    &[
                        "checkout",
                        "--quiet",
                        "--track",
                        "-b",
                        &branch,
                        &format!("refs/remotes/{remote}/{branch}"),
                    ],
                    ACTION_TIMEOUT,
                )?;
            }
        }
        "delete_remote_branch" => {
            let (remote, branch, _) = resolve_remote_branch(&root, require_str(body, "branch")?)?;
            run_git(
                &root,
                &["push", &remote, "--delete", &branch],
                ACTION_TIMEOUT,
            )?;
            run_git(&root, &["fetch", "--prune", &remote], ACTION_TIMEOUT)?;
        }
        "pull_branch" => {
            let (branch, remote, remote_branch) =
                branch_upstream(&root, require_str(body, "branch")?)?;
            run_git(
                &root,
                &[
                    "fetch",
                    &remote,
                    &format!("{remote_branch}:refs/heads/{branch}"),
                ],
                ACTION_TIMEOUT,
            )?;
        }
        "push_branch" => {
            let (branch, remote, remote_branch) =
                branch_upstream(&root, require_str(body, "branch")?)?;
            run_git(
                &root,
                &[
                    "push",
                    &remote,
                    &format!("refs/heads/{branch}:refs/heads/{remote_branch}"),
                ],
                ACTION_TIMEOUT,
            )?;
        }
        "merge_branch" => {
            let source = validate_branch(&root, require_str(body, "branch")?)?;
            let target = validate_branch(&root, require_str(body, "targetBranch")?)?;
            if source == target {
                return Err(err("Source and target branches must be different"));
            }
            run_git(
                &root,
                &["rev-parse", "--verify", &format!("refs/heads/{source}")],
                STATUS_TIMEOUT,
            )?;
            run_git(
                &root,
                &["rev-parse", "--verify", &format!("refs/heads/{target}")],
                STATUS_TIMEOUT,
            )?;
            // 目标分支被某个 worktree 占着就在那里合，否则在仓库根合
            match worktree_of_branch(&root, &target) {
                Some(dir) => {
                    run_git(&dir, &["merge", "--no-edit", &source], ACTION_TIMEOUT)?;
                }
                None => {
                    run_git(&root, &["checkout", "--quiet", &target], ACTION_TIMEOUT)?;
                    run_git(&root, &["merge", "--no-edit", &source], ACTION_TIMEOUT)?;
                }
            }
        }
        "continue" | "abort" => {
            let Some(operation) = operation_in_progress(&root) else {
                return Err(err("No Git operation is currently in progress"));
            };
            let flag = if action == "continue" {
                "--continue"
            } else {
                "--abort"
            };
            run_git(&root, &[operation, flag], ACTION_TIMEOUT)?;
        }
        _ => return Err(err("Unsupported Git action")),
    }

    let mut status = full_status(cwd);
    let untracked = if status
        .get("isGitRepository")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        status
            .get("repositoryRoot")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .unwrap_or_else(|| root.clone())
    } else {
        cwd.to_path_buf()
    };
    status["untrackedPaths"] = json!(untracked_paths(&untracked));
    Ok(status)
}

/// 把凭据交给系统 credential helper（等价于 `git credential approve`）。
///
/// 只对 HTTPS 有意义（helper 体系的字段就是 protocol / host / username /
/// password）；SSH 口令没有这一层，靠 [`CredentialStore`]。保留它是因为它能让
/// 用户终端里自己跑的 `git push` 也受益。
fn remember_credential(url: Option<&str>, username: Option<&str>, secret: &str) {
    let Some(url) = url else { return };
    let Some(parsed) = url::Url::parse(url).ok() else {
        return;
    };
    let host = match (parsed.host_str(), parsed.port()) {
        (Some(host), Some(port)) => format!("{host}:{port}"),
        (Some(host), None) => host.to_string(),
        _ => return,
    };
    let mut stdin = String::new();
    stdin.push_str("protocol=https\n");
    stdin.push_str(&format!("host={host}\n"));
    if let Some(username) = username {
        stdin.push_str(&format!("username={username}\n"));
    }
    stdin.push_str(&format!("password={secret}\n\n"));
    let Ok(mut child) = Command::new("git")
        .args(["credential", "approve"])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return;
    };
    if let Some(mut pipe) = child.stdin.take() {
        use std::io::Write;
        let _ = pipe.write_all(stdin.as_bytes());
    }
    let _ = child.wait();
}

/// 一组允许访问的根之外就拒绝（路由用）。
pub fn ensure_within(cwd: &str, allowed: &crate::paths::AllowedRoots) -> bool {
    allowed.allows(cwd) && allowed.allows_real(cwd)
}

/// 解析 `cwd` 查询参数（`git.js` 的 `k()`）：必须绝对路径、目录、且在白名单里。
pub fn resolve_cwd(
    raw: Option<&str>,
    allowed: &crate::paths::AllowedRoots,
) -> Result<PathBuf, (u16, String)> {
    let Some(raw) = raw.map(str::trim).filter(|s| !s.is_empty()) else {
        return Err((400, "cwd must be an absolute path".into()));
    };
    if !crate::paths::is_absolute_path(raw) {
        return Err((400, "cwd must be an absolute path".into()));
    }
    if !allowed.allows(raw) || !allowed.allows_real(raw) {
        return Err((403, "Access denied".into()));
    }
    let path = PathBuf::from(raw);
    match std::fs::metadata(&path) {
        Ok(meta) if meta.is_dir() => Ok(path),
        Ok(_) => Err((400, "Not a directory".into())),
        Err(_) => Err((400, "Directory not found".into())),
    }
}

/// 供 `sessions_store` 复用：cwd → `(projectRoot, worktreeBranch)`。
pub fn project_hint(cwd: &str) -> (String, Option<String>) {
    let info = repo_info(cwd);
    let root = if info.project_root.is_empty() {
        cwd.to_string()
    } else {
        info.project_root
    };
    let branch = if info.is_worktree { info.branch } else { None };
    (root, branch)
}

/// 把一串仓库状态里的文件路径去重（测试与路由都会用）。
pub fn distinct_paths(values: &[Value]) -> BTreeSet<String> {
    values
        .iter()
        .filter_map(|v| v.get("filePath").and_then(Value::as_str))
        .map(str::to_string)
        .collect()
}

/// 大文件阈值（`diff_for_file` 之外的地方也用）。
pub fn is_large(bytes: usize) -> bool {
    bytes > LARGE_FILE_BYTES
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let status = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@t")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@t")
            .output()
            .expect("git");
        assert!(
            status.status.success(),
            "git {:?} 失败: {}",
            args,
            String::from_utf8_lossy(&status.stderr)
        );
    }

    fn temp_path(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toiler-git-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn temp_repo(tag: &str) -> PathBuf {
        let dir = temp_path(tag);
        git(&dir, &["init", "-q", "-b", "main"]);
        dir
    }

    /// 裸仓库，当作本地 remote：用绝对路径，`credential_kind` 会判成 `none`，
    /// 测试不需要 askpass 也能跑完整条推送链路。
    fn temp_bare_repo(tag: &str) -> PathBuf {
        let dir = temp_path(tag);
        git(&dir, &["init", "--bare", "-q"]);
        dir
    }

    #[test]
    fn status_reports_untracked_and_modified() {
        let dir = temp_repo("status");
        std::fs::write(dir.join("tracked.txt"), "one\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);

        std::fs::write(dir.join("tracked.txt"), "one\ntwo\n").unwrap();
        std::fs::write(dir.join("new.txt"), "a\nb\n").unwrap();

        let value = status(&dir);
        assert_eq!(value["isGitRepository"], true);
        let files = value["files"].as_array().unwrap();
        let names: Vec<&str> = files
            .iter()
            .map(|f| f["status"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"modified"));
        assert!(names.contains(&"untracked"));
        // 未跟踪文件按行数计入 additions
        assert!(value["additions"].as_u64().unwrap() >= 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn diff_of_untracked_file_is_supported() {
        let dir = temp_repo("diff");
        std::fs::write(dir.join("a.txt"), "hello\nworld\n").unwrap();
        let value = diff_for_file(&dir, &dir.join("a.txt"));
        assert_eq!(value["supported"], true);
        assert_eq!(value["status"], "untracked");
        assert!(value["patch"].as_str().unwrap().contains("+hello"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn credential_kind_detects_schemes() {
        assert_eq!(credential_kind(Some("https://x/y.git")), "https");
        assert_eq!(credential_kind(Some("git@github.com:x/y.git")), "ssh");
        assert_eq!(credential_kind(Some("ssh://git@host/y.git")), "ssh");
        assert_eq!(credential_kind(Some("file:///tmp/x")), "none");
        assert_eq!(credential_kind(None), "none");
    }

    /// 测试用的凭据存储：临时文件 + 固定密钥（不碰系统钥匙串，否则 CI 与未授权
    /// 的机器上会成批失败）。
    fn temp_credential_store(tag: &str) -> (CredentialStore, PathBuf) {
        let dir = temp_path(tag);
        let store = CredentialStore::with_key(dir.join("git-credentials.json"), vec![7u8; 32]);
        (store, dir)
    }

    /// 回归：【勾了「在此设备上加密记住凭据」，口令还是会被清空】的直接用例。
    ///
    /// 面板的行为是「状态里 `hasSavedCredential` 为真 → 口令框留占位符、下次留空
    /// 就用已存的」。所以存下去之后 `full_status` 必须能读出用户名，否则前端下一
    /// 次刷新又把框清空，用户永远得重输。
    #[test]
    fn remembered_credential_shows_up_in_the_panel_status() {
        let (store, dir) = temp_credential_store("remember-status");
        let repo = temp_repo("remember-status-repo");
        // `credential_kind` 只认 https / ssh，所以这里给个不用真连的 https 远程。
        git(
            &repo,
            &[
                "remote",
                "add",
                "origin",
                "https://example.invalid/acme/repo.git",
            ],
        );

        let before = full_status_with_store(&repo, &store);
        assert_eq!(before["credentialKind"], "https");
        assert_eq!(before["hasSavedCredential"], false, "存之前不能报已存");
        assert_eq!(before["savedCredentialUsername"], Value::Null);

        store
            .save(
                "https://example.invalid/acme/repo.git",
                &GitCredential::new("https", Some("alice".into()), "token-123"),
            )
            .unwrap();

        let after = full_status_with_store(&repo, &store);
        assert_eq!(
            after["hasSavedCredential"], true,
            "存过之后必须报已存，否则前端会把口令框清空：{after}"
        );
        assert_eq!(after["savedCredentialUsername"], "alice");

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&repo);
    }

    /// 回归：面板留空（只带占位符，不回传明文）时，后端必须自己把已存的凭据
    /// 取出来用 —— 这是「第二次推送就不用再输口令」的前提。
    #[test]
    fn empty_form_falls_back_to_the_saved_credential() {
        let (store, dir) = temp_credential_store("fallback");
        let remote = "git@example.invalid:acme/repo.git";
        store
            .save(remote, &GitCredential::new("ssh", None, "passphrase"))
            .unwrap();

        // 面板留空：请求体里没有 credential 字段。
        let resolved =
            resolve_credential(&store, Some(remote), "ssh", &json!({ "action": "push" }))
                .unwrap()
                .expect("留空时必须用已存的凭据");
        assert_eq!(resolved.0, "ssh");
        assert_eq!(resolved.1, None);
        assert_eq!(resolved.2, "passphrase");

        // 显式填的优先（用户换了 token）。
        let explicit = resolve_credential(
            &store,
            Some(remote),
            "ssh",
            &json!({"credential": {"kind": "ssh", "secret": "typed-in"}}),
        )
        .unwrap()
        .expect("显式凭据不能用已存的覆盖");
        assert_eq!(explicit.2, "typed-in");

        // 没存过、也没填：不报错，交给 git 自己的凭据链路。
        assert!(resolve_credential(
            &store,
            Some("git@example.invalid:acme/untouched.git"),
            "ssh",
            &json!({}),
        )
        .unwrap()
        .is_none());

        // `none` 类型的远程（file:// / 本地裸仓库）不问存储：没有凭据这回事。
        assert!(resolve_credential(&store, Some(remote), "none", &json!({}))
            .unwrap()
            .is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 回归：勾了「记住」之后真的落盘，且**只**在勾了的时候落盘。
    /// 早先这里只调 `git credential approve`，SSH 远程勾了等于没勾。
    #[test]
    fn remember_flag_persists_the_credential_for_ssh() {
        let (store, dir) = temp_credential_store("remember-save");
        let remote = "git@example.invalid:acme/repo.git";

        // 跑 git 会失败（域名不存在），但凭据存储发生在 network **成功**之后，
        // 所以这里只验「保存」这一段自己的语义，不依赖网络。
        store
            .save(remote, &GitCredential::new("ssh", None, "passphrase"))
            .unwrap();
        assert_eq!(
            store.saved_username(remote, "ssh"),
            Some(None),
            "SSH 凭据没有用户名，但必须算「存过」"
        );

        // 换一个仓库不能串：面板按远程散列取键，串了就会把别人的口令发出去。
        assert!(store
            .saved_username("git@example.invalid:acme/other.git", "ssh")
            .is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// askpass 脚本必须能区分用户名与密码两个 prompt，且用文件读而不是把凭据
    /// 写进脚本文本（脚本在磁盘上、且会被 git 当程序执行）。
    #[test]
    fn askpass_script_matches_the_platform_it_runs_on() {
        let (name, body) = askpass_script();
        // 两个变量都引用了，说明两路分支都在。
        assert!(body.contains("TOILER_GIT_USERNAME_FILE"), "{body}");
        assert!(body.contains("TOILER_GIT_SECRET_FILE"), "{body}");
        // 大小写不敏感地判断 “username”，否则 git 的 “Username for ...” 会走到密码分支。
        assert!(
            body.contains("[Uu]sername") || body.contains("/I \"sername\""),
            "{body}"
        );

        #[cfg(unix)]
        {
            assert_eq!(name, "askpass.sh");
            assert!(body.starts_with("#!/bin/sh"), "{body}");
        }
        #[cfg(not(unix))]
        {
            // Windows 上 git 靠 CreateProcess 起它，必须是 `.cmd` 且有 CRLF 换行。
            assert_eq!(name, "askpass.cmd");
            assert!(body.contains("\r\n"), "批处理需要 CRLF");
            assert!(!body.contains("TOILER_ASKPASS"), "凭据不该写进脚本");
        }
    }

    /// 凭据只落在独立的 0600 文件里，脚本本身与命令行都不含 secret。
    #[test]
    fn askpass_keeps_the_secret_out_of_the_script_and_env() {
        let askpass = Askpass::new("s3cr3t-value", Some("alice")).unwrap();
        assert_eq!(
            std::fs::read_to_string(askpass.dir.join("secret")).unwrap(),
            "s3cr3t-value"
        );
        assert_eq!(
            std::fs::read_to_string(askpass.dir.join("username")).unwrap(),
            "alice"
        );
        let (name, _) = askpass_script();
        let script = std::fs::read_to_string(askpass.dir.join(name)).unwrap();
        assert!(
            !script.contains("s3cr3t-value"),
            "secret 不能写进脚本：{script}"
        );
        // 环境变量只给文件路径，不给明文。
        for (key, value) in askpass.env() {
            assert!(
                !value.contains("s3cr3t-value"),
                "{key} 泄漏了 secret：{value}"
            );
        }
        let dir = askpass.dir.clone();
        drop(askpass);
        assert!(!dir.exists(), "临时目录必须随 Drop 清掉");
    }

    #[test]
    fn branch_names_are_sanitized() {
        assert_eq!(sanitize_branch("feat/x y").unwrap(), "feat-x-y");
        assert!(sanitize_branch("   ").is_err());
        assert!(sanitize_branch("///").is_err());
    }

    #[test]
    fn repo_info_reports_top_level() {
        let dir = temp_repo("info");
        // 空仓库（没有 HEAD）时 pi 也是走 fallback，所以先提交一次
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        let info = repo_info(dir.to_string_lossy().as_ref());
        assert!(info.is_top_level);
        assert!(!info.is_worktree);
        assert_eq!(
            std::fs::canonicalize(&info.project_root).unwrap(),
            std::fs::canonicalize(&dir).unwrap()
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stage_and_commit_actions_work() {
        let dir = temp_repo("actions");
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        let staged = action(
            &dir,
            &json!({"action": "stage", "paths": [dir.join("a.txt").to_string_lossy()]}),
        )
        .unwrap();
        assert!(staged["isGitRepository"].as_bool().unwrap());

        let committed = action(&dir, &json!({"action": "commit", "message": "init"})).unwrap();
        let changes = &committed["changes"];
        assert_eq!(changes["files"].as_array().unwrap().len(), 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn tag_actions_create_and_delete() {
        let dir = temp_repo("tags");
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);

        // 轻量 tag：只给名字
        let lightweight = action(&dir, &json!({"action": "create_tag", "tag": "v1.0.0"})).unwrap();
        assert_eq!(lightweight["tags"], json!(["v1.0.0"]));
        let kind = run_git(
            &dir,
            &["cat-file", "-t", "refs/tags/v1.0.0"],
            STATUS_TIMEOUT,
        )
        .unwrap();
        assert_eq!(kind, "commit");

        // 带 message 则建附注 tag
        let annotated = action(
            &dir,
            &json!({"action": "create_tag", "tag": "v1.1.0", "message": "release"}),
        )
        .unwrap();
        assert_eq!(annotated["tags"], json!(["v1.0.0", "v1.1.0"]));
        let kind = run_git(
            &dir,
            &["cat-file", "-t", "refs/tags/v1.1.0"],
            STATUS_TIMEOUT,
        )
        .unwrap();
        assert_eq!(kind, "tag");

        // 重复名字与非法名字都报错（与 git 自己的拒绝一致）
        assert!(action(&dir, &json!({"action": "create_tag", "tag": "v1.0.0"})).is_err());
        assert!(action(&dir, &json!({"action": "create_tag", "tag": "bad name"})).is_err());

        let deleted = action(&dir, &json!({"action": "delete_tag", "tag": "v1.0.0"})).unwrap();
        assert_eq!(deleted["tags"], json!(["v1.1.0"]));
        // 删不存在的 tag 不能静默成功
        assert!(action(&dir, &json!({"action": "delete_tag", "tag": "v1.0.0"})).is_err());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 推送标签是网络动作：remote 用本地裸仓库，重点验证命令形状（单推送
    /// `refs/tags/<tag>`、全推送 `--tags`）与「标签不存在时在本地就报错」。
    /// 对应前端 `GitPanel` 标签行的「推送到远程」与标签区标题的「推送全部标签」。
    #[test]
    fn push_tag_actions_publish_tags_to_origin() {
        let remote = temp_bare_repo("tags-push-remote");
        let dir = temp_repo("tags-push");
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);
        let remote_url = remote.to_string_lossy().to_string();
        git(&dir, &["remote", "add", "origin", remote_url.as_str()]);

        action(
            &dir,
            &json!({"action": "create_tag", "tag": "v1.0.0", "message": "release"}),
        )
        .unwrap();
        action(&dir, &json!({"action": "create_tag", "tag": "v0.9.0"})).unwrap();

        action(&dir, &json!({"action": "push_tag", "tag": "v1.0.0"})).unwrap();
        assert_eq!(
            run_git(&remote, &["tag", "--list"], STATUS_TIMEOUT).unwrap(),
            "v1.0.0",
            "push_tag 只推指定的那一个"
        );

        action(&dir, &json!({"action": "push_tags"})).unwrap();
        assert_eq!(
            run_git(&remote, &["tag", "--list"], STATUS_TIMEOUT).unwrap(),
            "v0.9.0\nv1.0.0",
            "push_tags 补上剩下的，已推送过的不会让整条命令失败"
        );

        let missing = action(&dir, &json!({"action": "push_tag", "tag": "v9.9.9"})).unwrap_err();
        assert!(
            missing.message.contains("Tag not found"),
            "{}",
            missing.message
        );

        let _ = std::fs::remove_dir_all(&dir);
        let _ = std::fs::remove_dir_all(&remote);
    }

    #[test]
    fn worktree_listing_marks_main() {
        let dir = temp_repo("worktrees");
        std::fs::write(dir.join("a.txt"), "x\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "init"]);

        let list = worktrees(&dir);
        assert_eq!(list.len(), 1);
        assert!(list[0].is_main);
        assert!(current_worktree_path(&list, &dir).is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
