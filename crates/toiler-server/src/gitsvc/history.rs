//! 工作树历史与引用操作；浏览接口只读本地引用和提交对象。

pub(crate) mod refs_action;

use super::{
    err, is_binary, operation_in_progress, parse_porcelain, read_git_stdout, repo_lock,
    run_git_inner, synthesize_patch, GitError, ACTION_TIMEOUT, MAX_DIFF_FILE_BYTES, STATUS_TIMEOUT,
};
use serde::Serialize;
use std::io::{self, BufRead, BufReader};
use std::path::Path;

const REF_PREFIXES: [&str; 3] = ["refs/heads/", "refs/remotes/", "refs/tags/"];
pub(crate) const PAGE_SIZE: usize = 100;
const SUMMARY_FORMAT: &str = "--format=%H%x00%P%x00%an%x00%aI%x00%s%x00%ae";
const SEARCH_FORMAT: &str = "--format=%H%x00%P%x00%an%x00%aI%x00%s%x00%ae%x00%at";
const DETAIL_FORMAT: &str = "--format=%H%x00%P%x00%an%x00%aI%x00%s%x00%ae%x00%B";
const LOG_OPTIONS: &[&str] = &[
    "--no-patch",
    "--no-decorate",
    "--no-show-signature",
    "--no-notes",
    "--no-color",
    "--encoding=UTF-8",
    "-z",
];

fn history_args<'a>(args: &[&'a str]) -> Vec<&'a str> {
    [
        vec!["--no-replace-objects", "--literal-pathspecs"],
        args.to_vec(),
    ]
    .concat()
}

fn read_only_env() -> [(&'static str, String); 3] {
    [
        ("GIT_NO_LAZY_FETCH", "1".into()),
        ("GIT_ALLOW_PROTOCOL", String::new()),
        ("GIT_OPTIONAL_LOCKS", "0".into()),
    ]
}

fn read(root: &Path, args: &[&str]) -> Result<String, GitError> {
    run_git_inner(
        root,
        &history_args(args),
        STATUS_TIMEOUT,
        &read_only_env(),
        false,
    )
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Refs {
    repository_root: String,
    head_branch: Option<String>,
    refs: Vec<Reference>,
    remotes: Vec<String>,
    upstreams: std::collections::BTreeMap<String, Upstream>,
}

#[derive(Serialize)]
struct Upstream {
    remote: String,
    branch: String,
}

#[derive(Serialize)]
struct Reference {
    name: String,
    commit: Option<String>,
}

fn head_branch(root: &Path) -> Option<String> {
    read(root, &["symbolic-ref", "--quiet", "HEAD"])
        .ok()
        .map(|branch| branch.trim_end().to_owned())
}

pub(crate) fn refs(root: &Path) -> Result<Refs, GitError> {
    let raw = read(
        root,
        &[
            "for-each-ref",
            "--sort=refname",
            "--format=%(refname)%00%(symref)%00%(objecttype)%00%(objectname)%00%(*objecttype)%00%(*objectname)%00%(upstream:remotename)%00%(upstream:remoteref)",
            "refs/heads",
            "refs/remotes",
            "refs/tags",
        ],
    )?;
    let mut upstreams = std::collections::BTreeMap::new();
    let refs = raw
        .lines()
        .filter_map(|line| {
            let [name, symref, kind, oid, target_kind, target, remote, upstream] =
                line.split('\0').collect::<Vec<_>>().try_into().ok()?;
            if name.starts_with(REF_PREFIXES[0]) && !remote.is_empty() {
                if let Some(branch) = upstream.strip_prefix(REF_PREFIXES[0]) {
                    upstreams.insert(
                        name.into(),
                        Upstream {
                            remote: remote.into(),
                            branch: branch.into(),
                        },
                    );
                }
            }
            // origin/HEAD 等只是同组分支的别名，不是一条新分支。
            symref.is_empty().then(|| Reference {
                name: name.into(),
                commit: match (kind, target_kind) {
                    ("commit", _) => Some(oid.into()),
                    (_, "commit") => Some(target.into()),
                    _ => None,
                },
            })
        })
        .collect();
    Ok(Refs {
        repository_root: root.to_string_lossy().into_owned(),
        head_branch: head_branch(root),
        refs,
        remotes: refs_action::remotes(root)?,
        upstreams,
    })
}

/// 使用工作树生效的 Git 邮箱，而不是靠姓名判断是否为自己的提交。
fn configured_author_email(root: &Path) -> Option<String> {
    let email = read(root, &["config", "--get", "user.email"]).ok()?;
    let email = email.trim();
    (!email.is_empty()).then(|| email.to_owned())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct Summary {
    hash: String,
    parents: Vec<String>,
    author: String,
    is_own_author: bool,
    authored_at: String,
    subject: String,
}

impl Summary {
    fn parse(fields: &[&str], own_email: Option<&str>) -> Result<Self, GitError> {
        match fields {
            [hash, parents, author, authored_at, subject, email, ..] if is_oid(hash) => Ok(Self {
                hash: (*hash).into(),
                parents: parents.split_whitespace().map(str::to_owned).collect(),
                author: (*author).into(),
                is_own_author: own_email.is_some_and(|own| own.eq_ignore_ascii_case(email)),
                authored_at: (*authored_at).into(),
                subject: (*subject).into(),
            }),
            _ => Err(err("Invalid Git commit output")),
        }
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Page {
    start: Option<String>,
    commits: Vec<Summary>,
    next_offset: Option<usize>,
}

fn is_oid(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn resolve_commit(root: &Path, reference: &str) -> Result<String, GitError> {
    let hash = read(
        root,
        &[
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ],
    )?;
    let hash = hash.trim_end();
    if !is_oid(hash) {
        return Err(err("Invalid Git commit output"));
    }
    Ok(hash.into())
}

fn exact_commit(root: &Path, hash: &str) -> Result<String, GitError> {
    if !is_oid(hash) {
        return Err(err("Expected a full commit hash"));
    }
    let resolved = resolve_commit(root, hash)?;
    if !resolved.eq_ignore_ascii_case(hash) {
        return Err(err("Expected a commit object"));
    }
    Ok(resolved)
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum ResetMode {
    Soft,
    Mixed,
    Hard,
    Keep,
}

#[derive(serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub(crate) enum HistoryAction {
    CherryPick,
    Reset { mode: ResetMode },
}

pub(crate) fn apply_action(
    root: &Path,
    action: HistoryAction,
    hash: &str,
    expected_branch: &str,
    expected_head: &str,
) -> Result<(), GitError> {
    let lock = repo_lock(root);
    let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
    let target = exact_commit(root, hash)?;
    // 浏览的是任意引用；实际操作始终限于用户发起操作时看到的检出分支和 HEAD。
    let branch = head_branch(root)
        .filter(|branch| branch.starts_with("refs/heads/"))
        .ok_or_else(|| err("No local branch is checked out"))?;
    if branch != expected_branch
        || !resolve_commit(root, "HEAD")?.eq_ignore_ascii_case(expected_head)
    {
        return Err(err(
            "The checked-out branch or HEAD changed; refresh history and try again",
        ));
    }
    if operation_in_progress(root).is_some() {
        return Err(err("Finish or abort the current Git operation first"));
    }
    let args = match action {
        HistoryAction::CherryPick => vec!["cherry-pick", &target],
        HistoryAction::Reset { mode } => {
            let flag = match mode {
                ResetMode::Soft => "--soft",
                ResetMode::Mixed => "--mixed",
                ResetMode::Hard => "--hard",
                ResetMode::Keep => "--keep",
            };
            vec!["reset", flag, &target, "--"]
        }
    };
    run_git_inner(
        root,
        &history_args(&args),
        ACTION_TIMEOUT,
        &read_only_env(),
        true,
    )?;
    Ok(())
}

#[derive(Default)]
pub(crate) struct HistoryFilters {
    commit_id: String,
    subject: String,
    author: String,
    from: Option<i64>,
    before: Option<i64>,
}

impl HistoryFilters {
    pub(crate) fn new(
        commit_id: &str,
        subject: &str,
        author: &str,
        from: Option<i64>,
        before: Option<i64>,
    ) -> Result<Self, GitError> {
        let commit_id = commit_id.trim();
        if !commit_id.is_empty()
            && (commit_id.len() != 8 || !commit_id.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(err("Commit ID must be exactly 8 hexadecimal characters"));
        }
        if matches!((from, before), (Some(from), Some(before)) if from >= before) {
            return Err(err("Invalid commit date range"));
        }
        Ok(Self {
            commit_id: commit_id.to_ascii_lowercase(),
            subject: subject.trim().to_lowercase(),
            author: author.trim().to_lowercase(),
            from,
            before,
        })
    }

    fn is_empty(&self) -> bool {
        self.commit_id.is_empty()
            && self.subject.is_empty()
            && self.author.is_empty()
            && self.from.is_none()
            && self.before.is_none()
    }

    fn matches(&self, fields: &[Vec<u8>; 7]) -> bool {
        if !fields[0].starts_with(self.commit_id.as_bytes())
            || (!self.subject.is_empty()
                && !String::from_utf8_lossy(&fields[4])
                    .to_lowercase()
                    .contains(&self.subject))
            || (!self.author.is_empty()
                && !String::from_utf8_lossy(&fields[2])
                    .to_lowercase()
                    .contains(&self.author))
        {
            return false;
        }
        if self.from.is_none() && self.before.is_none() {
            return true;
        }
        String::from_utf8_lossy(&fields[6])
            .parse::<i64>()
            .ok()
            .is_some_and(|at| {
                !matches!(self.from, Some(from) if at < from)
                    && !matches!(self.before, Some(before) if at >= before)
            })
    }
}

fn search(
    root: &Path,
    start: &str,
    offset: usize,
    filters: HistoryFilters,
    own_email: Option<String>,
) -> Result<Vec<Summary>, GitError> {
    let mut args = vec!["log", "--topo-order", SEARCH_FORMAT];
    args.extend(LOG_OPTIONS);
    args.extend([start, "--"]);
    // 摘要是 %s，提交时间是 %at，不能用会匹配正文或提交者时间的 Git 选项代替。
    // 流式读取而不是把整个仓库的日志攒起来，offset 只计算匹配的提交。
    read_git_stdout(
        root,
        &history_args(&args),
        STATUS_TIMEOUT,
        &read_only_env(),
        move |stdout| {
            let mut reader = BufReader::new(stdout);
            let mut fields: [Vec<u8>; 7] = Default::default();
            let mut skipped = 0;
            let mut commits = Vec::new();
            loop {
                for (index, field) in fields.iter_mut().enumerate() {
                    field.clear();
                    let size = reader
                        .read_until(0, field)
                        .map_err(|error| err(error.to_string()))?;
                    if size == 0 && index == 0 {
                        return Ok(commits);
                    }
                    if field.pop() != Some(0) {
                        return Err(err("Invalid Git commit output"));
                    }
                }
                if !filters.matches(&fields) {
                    continue;
                }
                if skipped < offset {
                    skipped += 1;
                    continue;
                }
                let text = fields
                    .each_ref()
                    .map(|field| String::from_utf8_lossy(field));
                commits.push(Summary::parse(
                    &text.each_ref().map(|field| field.as_ref()),
                    own_email.as_deref(),
                )?);
                if commits.len() > PAGE_SIZE {
                    // 仍要读完管道，让 Git 正常退出并保留真实错误。
                    io::copy(&mut reader, &mut io::sink())
                        .map_err(|error| err(error.to_string()))?;
                    return Ok(commits);
                }
            }
        },
    )
}

pub(crate) fn page(
    root: &Path,
    reference: &str,
    start: Option<&str>,
    offset: usize,
    filters: HistoryFilters,
) -> Result<Page, GitError> {
    let start = if let Some(start) = start {
        // 页码绑定到首屏的提交，不随分支指针移动而漏行或产生重复项。
        exact_commit(root, start)?
    } else {
        if offset != 0 {
            return Err(err("Pagination requires a starting commit"));
        }
        if reference != "HEAD" {
            if !REF_PREFIXES
                .iter()
                .any(|prefix| reference.starts_with(prefix))
            {
                return Err(err("Invalid Git reference"));
            }
            read(root, &["check-ref-format", reference])
                .map_err(|_| err("Invalid Git reference"))?;
        }
        match resolve_commit(root, reference) {
            Ok(hash) => hash,
            Err(error) => {
                // HEAD 指向还没有创建的本地分支是空仓库，不是加载失败。
                if reference == "HEAD"
                    && head_branch(root).is_some_and(|branch| {
                        read(root, &["show-ref", "--verify", "--quiet", &branch]).is_err()
                    })
                {
                    return Ok(Page {
                        start: None,
                        commits: Vec::new(),
                        next_offset: None,
                    });
                }
                return Err(error);
            }
        }
    };
    if offset > (i32::MAX as usize) - PAGE_SIZE {
        return Err(err("Git history offset is out of range"));
    }
    let own_email = configured_author_email(root);
    let mut commits = if filters.is_empty() {
        unfiltered(root, &start, offset, own_email.as_deref())?
    } else {
        search(root, &start, offset, filters, own_email)?
    };
    let next_offset = (commits.len() > PAGE_SIZE).then(|| offset + PAGE_SIZE);
    commits.truncate(PAGE_SIZE);
    Ok(Page {
        start: Some(start),
        commits,
        next_offset,
    })
}

fn unfiltered(
    root: &Path,
    start: &str,
    offset: usize,
    own_email: Option<&str>,
) -> Result<Vec<Summary>, GitError> {
    let skip = format!("--skip={offset}");
    let count = format!("--max-count={}", PAGE_SIZE + 1);
    let mut args = vec!["log", "--topo-order", SUMMARY_FORMAT, &skip, &count];
    args.extend(LOG_OPTIONS);
    args.extend([start, "--"]);
    let raw = read(root, &args)?;
    let fields = raw.split_terminator('\0').collect::<Vec<_>>();
    if fields.len() % 6 != 0 {
        return Err(err("Invalid Git commit output"));
    }
    fields
        .chunks(6)
        .map(|fields| Summary::parse(fields, own_email))
        .collect()
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ChangedFile {
    status: String,
    path: String,
    old_path: Option<String>,
}

fn parse_files(raw: &str) -> Result<Vec<ChangedFile>, GitError> {
    let mut fields = raw.split_terminator('\0');
    let mut files = Vec::new();
    while let Some(status) = fields.next() {
        let path = fields.next().ok_or_else(|| err("Invalid Git file list"))?;
        let (path, old_path) = if status.starts_with(['R', 'C']) {
            (
                fields.next().ok_or_else(|| err("Invalid Git file list"))?,
                Some(path.into()),
            )
        } else {
            (path, None)
        };
        files.push(ChangedFile {
            status: status.into(),
            path: path.into(),
            old_path,
        });
    }
    Ok(files)
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct ContainingBranch {
    name: String,
    is_tip: bool,
}

fn containing_branches(root: &Path, hash: &str) -> Result<Vec<ContainingBranch>, GitError> {
    // 由 Git 检查祖先关系，而不是从当前筛选或已加载的历史页推断。
    let raw = read(
        root,
        &[
            "for-each-ref",
            "--contains",
            hash,
            "--sort=refname",
            "--format=%(refname)%00%(symref)%00%(objectname)",
            "refs/heads/",
            "refs/remotes/",
        ],
    )?;
    Ok(raw
        .lines()
        .filter_map(|line| {
            let (name, rest) = line.split_once('\0')?;
            let (symref, oid) = rest.split_once('\0')?;
            // 远程 HEAD 等符号引用只是分支别名，避免重复计数。
            symref.is_empty().then(|| ContainingBranch {
                name: name.into(),
                is_tip: oid == hash,
            })
        })
        .collect())
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Commit {
    #[serde(flatten)]
    summary: Summary,
    author_email: String,
    message: String,
    files: Vec<ChangedFile>,
    containing_branches: Vec<ContainingBranch>,
}

#[derive(serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum DiffScope {
    Unstaged,
    Staged,
    Commit,
}

#[derive(Serialize)]
pub(crate) struct WorkspaceDiff {
    patch: String,
    oversized: bool,
}

const PATCH_OPTIONS: &[&str] = &[
    "--patch",
    "--no-color",
    "--no-ext-diff",
    "--no-textconv",
    "--no-relative",
    "--src-prefix=a/",
    "--dst-prefix=b/",
    "--unified=3",
    "-M",
];

fn git_path(path: &str) -> Result<&str, GitError> {
    if path.contains('\0') || path.split('/').any(|part| matches!(part, "" | "." | "..")) {
        return Err(err("Invalid repository path"));
    }
    Ok(path)
}

pub(crate) fn workspace_diff(
    root: &Path,
    scope: DiffScope,
    path: &str,
    hash: Option<&str>,
    old_path: Option<&str>,
) -> Result<WorkspaceDiff, GitError> {
    let patch = match scope {
        DiffScope::Commit => {
            let hash = exact_commit(root, hash.ok_or_else(|| err("Missing commit hash"))?)?;
            let parent = read(
                root,
                &[
                    "show",
                    "--no-patch",
                    "--no-notes",
                    "--no-show-signature",
                    "--format=%P",
                    &hash,
                    "--",
                ],
            )?;
            let mut args = vec!["diff-tree", "--no-commit-id", "-r"];
            args.extend(PATCH_OPTIONS);
            args.push(parent.split_whitespace().next().unwrap_or("--root"));
            args.extend([&hash, "--", git_path(path)?]);
            if let Some(old) = old_path.filter(|old| *old != path) {
                args.push(git_path(old)?);
            }
            read(root, &args)?
        }
        DiffScope::Staged | DiffScope::Unstaged => {
            let path = crate::paths::simplified_path(Path::new(path));
            let root = crate::paths::simplified_path(root);
            let relative = path
                .strip_prefix(&root)
                .map_err(|_| err("File is outside the repository"))?;
            let relative = relative.to_string_lossy();
            let relative = if cfg!(windows) {
                relative.replace('\\', "/").into()
            } else {
                relative
            };
            let relative = git_path(&relative)?;
            let staged = matches!(scope, DiffScope::Staged);
            let entries = parse_porcelain(&read(
                &root,
                &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
            )?);
            let Some(entry) = entries.iter().find(|entry| entry.path == relative) else {
                return Ok(WorkspaceDiff {
                    patch: String::new(),
                    oversized: false,
                });
            };
            if !staged && entry.index == '?' && entry.worktree == '?' {
                // Untracked 文件没有 Git 对象；只读仓库内的普通文件，不跟随符号链接。
                let metadata =
                    std::fs::symlink_metadata(&path).map_err(|error| err(error.to_string()))?;
                if !metadata.is_file() || !super::is_inside(&root, &path) {
                    return Ok(WorkspaceDiff {
                        patch: String::new(),
                        oversized: false,
                    });
                }
                if metadata.len() > MAX_DIFF_FILE_BYTES {
                    return Ok(WorkspaceDiff {
                        patch: String::new(),
                        oversized: true,
                    });
                }
                let bytes = std::fs::read(&path).map_err(|error| err(error.to_string()))?;
                if bytes.is_empty() || is_binary(&bytes) {
                    String::new()
                } else {
                    synthesize_patch(relative, &String::from_utf8_lossy(&bytes))
                }
            } else {
                let mut args = vec!["diff"];
                args.extend(PATCH_OPTIONS);
                if staged {
                    args.push("--cached");
                }
                args.extend(["--", relative]);
                let status = if staged { entry.index } else { entry.worktree };
                if matches!(status, 'R' | 'C') {
                    if let Some(old) = &entry.original {
                        args.push(git_path(old)?);
                    }
                }
                read(&root, &args)?
            }
        }
    };
    let oversized = patch.len() as u64 > MAX_DIFF_FILE_BYTES;
    Ok(WorkspaceDiff {
        patch: if oversized { String::new() } else { patch },
        oversized,
    })
}

pub(crate) fn commit(root: &Path, hash: &str) -> Result<Commit, GitError> {
    let hash = exact_commit(root, hash)?;
    let mut args = vec!["show", DETAIL_FORMAT];
    args.extend(LOG_OPTIONS);
    args.extend([hash.as_str(), "--"]);
    let data = read(root, &args)?;
    let fields = data
        .strip_suffix('\0')
        .unwrap_or(&data)
        .splitn(7, '\0')
        .collect::<Vec<_>>();
    let summary = Summary::parse(&fields, configured_author_email(root).as_deref())?;
    let [.., author_email, message] = fields.as_slice() else {
        return Err(err("Invalid Git commit output"));
    };
    // 合并提交与第一父提交比较（即合入目标分支前后的差别）；根提交与空树比较。
    let mut args = vec![
        "diff-tree",
        "--no-commit-id",
        "--name-status",
        "-z",
        "-r",
        "-M",
        "--no-ext-diff",
        "--no-textconv",
        "--no-color",
    ];
    if let Some(parent) = summary.parents.first() {
        args.push(parent);
    } else {
        args.push("--root");
    }
    args.extend([hash.as_str(), "--"]);
    let files = parse_files(&read(root, &args)?)?;
    Ok(Commit {
        summary,
        author_email: (*author_email).into(),
        message: message.trim_end_matches('\n').into(),
        files,
        containing_branches: containing_branches(root, &hash)?,
    })
}
