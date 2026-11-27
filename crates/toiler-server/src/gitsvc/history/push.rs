//! 推送预览只比较本地提交与远程跟踪引用；确认时只推送已预览的单个分支。

use super::{
    configured_author_email, head_branch, read, refs_action, resolve_commit, unfiltered, Summary,
    PAGE_SIZE,
};
use crate::gitsvc::{err, repo_lock, run_remote_action_for_url, GitError};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::path::Path;

const LOCAL: &str = "refs/heads/";
const STALE: &str = "The push target or commits changed; reload the push preview and try again";

struct Target {
    branch: String,
    remote: String,
    target_branch: String,
    url: String,
    head: String,
    base: Option<String>,
}

impl Target {
    fn resolve(root: &Path) -> Result<Self, GitError> {
        let reference = head_branch(root).ok_or_else(|| err("No local branch is checked out"))?;
        let branch = reference
            .strip_prefix(LOCAL)
            .ok_or_else(|| err("No local branch is checked out"))?;
        let head = resolve_commit(root, &reference)
            .map_err(|_| err("The checked-out branch has no commits"))?;
        let raw = read(
            root,
            &[
                "for-each-ref",
                "--format=%(upstream:remotename)%00%(upstream:remoteref)%00%(upstream)",
                &reference,
            ],
        )?;
        let mut upstream = raw.lines().next().unwrap_or_default().split('\0');
        let upstream_remote = upstream.next().unwrap_or_default();
        let upstream_branch = upstream.next().and_then(|value| value.strip_prefix(LOCAL));
        let upstream_ref = upstream.next().filter(|value| !value.is_empty());
        let remotes = refs_action::remotes(root)?;
        let config = |key: &str| {
            read(root, &["config", "--get", key])
                .ok()
                .and_then(|value| {
                    value
                        .lines()
                        .next()
                        .filter(|value| !value.is_empty())
                        .map(str::to_owned)
                })
        };
        let remote = config(&format!("branch.{branch}.pushRemote"))
            .or_else(|| config("remote.pushDefault"))
            .or_else(|| {
                (!upstream_remote.is_empty() && upstream_remote != ".")
                    .then(|| upstream_remote.to_owned())
            })
            .or_else(|| {
                remotes
                    .iter()
                    .find(|name| name.as_str() == "origin")
                    .cloned()
            })
            .or_else(|| (remotes.len() == 1).then(|| remotes[0].clone()))
            .ok_or_else(|| err("No push remote is configured for this branch"))?;
        if remote.starts_with('-') || !remotes.contains(&remote) {
            return Err(err("The push remote is not configured"));
        }
        // 不使用可能指向多分支/标签的默认 refspec。远端与上游一致时保留上游分支名。
        let tracks_upstream = remote == upstream_remote && upstream_branch.is_some();
        let target_branch = if tracks_upstream {
            upstream_branch.unwrap_or(branch)
        } else {
            branch
        };
        let tracking_ref = format!("refs/remotes/{remote}/{target_branch}");
        let base = resolve_commit(
            root,
            if tracks_upstream {
                upstream_ref.unwrap_or(&tracking_ref)
            } else {
                &tracking_ref
            },
        )
        .ok();
        let urls = read(root, &["remote", "get-url", "--push", "--all", &remote])?;
        let mut urls = urls.lines().filter(|value| !value.is_empty());
        let url = urls
            .next()
            .ok_or_else(|| err("The push remote has no URL"))?;
        if urls.next().is_some() {
            return Err(err("The push remote has multiple destinations"));
        }
        Ok(Self {
            branch: branch.into(),
            remote,
            target_branch: target_branch.into(),
            url: url.into(),
            head,
            base,
        })
    }

    fn token(&self) -> String {
        let fields = [
            &self.branch,
            &self.remote,
            &self.target_branch,
            &self.url,
            &self.head,
        ];
        let mut digest = Sha256::new();
        for field in fields {
            digest.update(field.as_bytes());
            digest.update([0]);
        }
        digest.update(self.base.as_deref().unwrap_or_default().as_bytes());
        format!("{:x}", digest.finalize())
    }

    fn uses_panel_credential(&self, root: &Path) -> bool {
        self.remote == "origin"
            && read(root, &["remote", "get-url", "origin"])
                .is_ok_and(|url| url.lines().next() == Some(self.url.as_str()))
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct Preview {
    branch: String,
    remote: String,
    target_branch: String,
    token: String,
    has_tracking_ref: bool,
    uses_panel_credential: bool,
    commits: Vec<Summary>,
    next_offset: Option<usize>,
}

pub(crate) fn preview(
    root: &Path,
    offset: usize,
    expected: Option<&str>,
) -> Result<Preview, GitError> {
    if offset > (i32::MAX as usize) - PAGE_SIZE || (offset != 0 && expected.is_none()) {
        return Err(err("Invalid push preview offset"));
    }
    let lock = repo_lock(root);
    let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
    let target = Target::resolve(root)?;
    let token = target.token();
    if expected.is_some_and(|expected| expected != token) {
        return Err(err(STALE));
    }
    let own_email = configured_author_email(root);
    let mut commits = unfiltered(
        root,
        &target.head,
        target.base.as_deref(),
        offset,
        own_email.as_deref(),
    )?;
    let next_offset = (commits.len() > PAGE_SIZE).then_some(offset + PAGE_SIZE);
    commits.truncate(PAGE_SIZE);
    Ok(Preview {
        token,
        uses_panel_credential: target.uses_panel_credential(root),
        branch: target.branch,
        remote: target.remote,
        target_branch: target.target_branch,
        has_tracking_ref: target.base.is_some(),
        commits,
        next_offset,
    })
}

/// 调用方已持有仓库锁。使用完整提交哈希而不是活动引用，避免推送未预览的提交。
pub(crate) fn confirm(root: &Path, body: &Value, expected: &str) -> Result<(), GitError> {
    let target = Target::resolve(root)?;
    if target.token() != expected {
        return Err(err(STALE));
    }
    if body.get("credential").is_some_and(|value| !value.is_null())
        && !target.uses_panel_credential(root)
    {
        return Err(err("The panel credential is not for this push destination"));
    }
    let refspec = format!("{}:{LOCAL}{}", target.head, target.target_branch);
    run_remote_action_for_url(
        root,
        &[
            "push",
            "--no-force",
            "--no-follow-tags",
            "--recurse-submodules=no",
            "--",
            &target.remote,
            &refspec,
        ],
        body,
        &Default::default(),
        Some(&target.url),
    )
}
