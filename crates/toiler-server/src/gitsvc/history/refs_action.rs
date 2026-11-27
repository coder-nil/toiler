//! 历史列表的写操作只接收完整引用，避免本地分支、远程分支和标签同名时选错对象。

use super::{head_branch, history_args, read, read_only_env, resolve_commit};
use crate::gitsvc::{
    credential_kind, err, operation_in_progress, repo_lock, resolve_credential, run_git_inner,
    run_network, worktree_of_branch, GitError, ACTION_TIMEOUT,
};
use serde::Deserialize;
use std::path::Path;

const LOCAL: &str = "refs/heads/";
const REMOTE: &str = "refs/remotes/";
const TAG: &str = "refs/tags/";

#[derive(Deserialize)]
#[serde(
    tag = "action",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum RefAction {
    Checkout,
    CreateBranch {
        new_branch: String,
    },
    Merge {
        expected_branch: String,
    },
    Update,
    Push {
        remote: String,
        target_branch: String,
    },
    Rename {
        new_branch: String,
    },
    Delete,
    PushTag,
}

pub(super) fn remotes(root: &Path) -> Result<Vec<String>, GitError> {
    Ok(read(root, &["remote"])?
        .lines()
        .map(str::to_owned)
        .collect())
}

fn mutate(root: &Path, args: &[&str]) -> Result<(), GitError> {
    run_git_inner(
        root,
        &history_args(args),
        ACTION_TIMEOUT,
        &read_only_env(),
        true,
    )
    .map(|_| ())
}

fn branch_name(root: &Path, name: &str) -> Result<(), GitError> {
    // --branch 单独使用会展开 @{-1}，必须同时要求它是原样合法的路径。
    if name.starts_with('-') {
        return Err(err("Invalid branch name"));
    }
    read(root, &["check-ref-format", &format!("{LOCAL}{name}")])?;
    read(root, &["check-ref-format", "--branch", name])?;
    Ok(())
}

fn check_remote(root: &Path, name: &str) -> Result<(), GitError> {
    if name.starts_with('-') || !remotes(root)?.iter().any(|remote| remote == name) {
        return Err(err("Remote is not configured"));
    }
    Ok(())
}

fn network(root: &Path, remote: &str, push: bool, args: &[&str]) -> Result<(), GitError> {
    check_remote(root, remote)?;
    let mut url_args = vec!["remote", "get-url"];
    if push {
        url_args.push("--push");
    }
    url_args.push(remote);
    let url = read(root, &url_args)?;
    let url = url.lines().next().ok_or_else(|| err("Remote has no URL"))?;
    let kind = credential_kind(Some(url));
    let credential = resolve_credential(
        &Default::default(),
        Some(url),
        kind,
        &serde_json::Value::Null,
    )?;
    run_network(root, args, credential, Some(url)).map(|_| ())
}

fn remote_parts<'a>(root: &Path, name: &'a str) -> Result<(String, &'a str), GitError> {
    let remote = remotes(root)?
        .into_iter()
        .filter(|remote| {
            !remote.starts_with('-')
                && name
                    .strip_prefix(remote.as_str())
                    .is_some_and(|rest| rest.starts_with('/'))
        })
        .max_by_key(String::len)
        .ok_or_else(|| err("Remote is not configured"))?;
    let branch = &name[remote.len() + 1..];
    branch_name(root, branch)?;
    Ok((remote, branch))
}

fn upstream(root: &Path, reference: &str) -> Result<(String, String), GitError> {
    let raw = read(
        root,
        &[
            "for-each-ref",
            "--format=%(upstream:remotename)%00%(upstream:remoteref)",
            reference,
        ],
    )?;
    let (remote, branch) = raw
        .trim_end()
        .split_once('\0')
        .ok_or_else(|| err("Branch has no remote upstream"))?;
    let branch = branch
        .strip_prefix(LOCAL)
        .filter(|_| !remote.is_empty())
        .ok_or_else(|| err("Branch has no remote upstream"))?;
    Ok((remote.into(), branch.into()))
}

fn checkout(root: &Path, branch: &str) -> Result<(), GitError> {
    mutate(
        root,
        &[
            "checkout",
            "--quiet",
            "--no-guess",
            "--no-overwrite-ignore",
            branch,
            "--",
        ],
    )
}

fn create_branch(root: &Path, name: &str, reference: &str, track: bool) -> Result<(), GitError> {
    branch_name(root, name)?;
    mutate(
        root,
        &[
            "checkout",
            "--quiet",
            "--no-overwrite-ignore",
            if track { "--track" } else { "--no-track" },
            "-b",
            name,
            reference,
            "--",
        ],
    )
}

pub(crate) fn apply(root: &Path, reference: &str, action: RefAction) -> Result<(), GitError> {
    let local = reference.strip_prefix(LOCAL);
    let remote = reference.strip_prefix(REMOTE);
    let tag = reference.strip_prefix(TAG);
    if local.is_none() && remote.is_none() && tag.is_none() {
        return Err(err("Expected a local branch, remote branch, or tag"));
    }
    let lock = repo_lock(root);
    let _guard = lock.lock().unwrap_or_else(|error| error.into_inner());
    read(root, &["check-ref-format", reference])?;
    let oid = read(root, &["show-ref", "--verify", "--hash", reference])?;
    if read(root, &["symbolic-ref", "--quiet", reference]).is_ok() {
        return Err(err("Symbolic references are not supported"));
    }
    let head = head_branch(root);
    if matches!(
        action,
        RefAction::Checkout
            | RefAction::CreateBranch { .. }
            | RefAction::Merge { .. }
            | RefAction::Update
    ) && operation_in_progress(root).is_some()
    {
        return Err(err("Finish or abort the current Git operation first"));
    }
    match action {
        RefAction::Checkout => {
            if let Some(branch) = local {
                branch_name(root, branch)?;
                checkout(root, branch)
            } else if let Some(name) = remote {
                let (server, branch) = remote_parts(root, name)?;
                let local_ref = format!("{LOCAL}{branch}");
                if read(root, &["show-ref", "--verify", "--quiet", &local_ref]).is_ok() {
                    if upstream(root, &local_ref).ok().as_ref()
                        != Some(&(server, branch.to_owned()))
                    {
                        return Err(err("A local branch with this name already exists; create a branch with another name"));
                    }
                    checkout(root, branch)
                } else {
                    create_branch(root, branch, reference, true)
                }
            } else {
                let commit = resolve_commit(root, reference)?;
                mutate(
                    root,
                    &[
                        "checkout",
                        "--quiet",
                        "--detach",
                        "--no-overwrite-ignore",
                        &commit,
                        "--",
                    ],
                )
            }
        }
        RefAction::CreateBranch { new_branch } if tag.is_none() => {
            create_branch(root, &new_branch, reference, remote.is_some())
        }
        RefAction::Merge { expected_branch } => {
            if !expected_branch.starts_with(LOCAL)
                || head.as_deref() != Some(&expected_branch)
                || reference == expected_branch
            {
                return Err(err("The merge target must be the currently checked-out local branch and differ from the source"));
            }
            resolve_commit(root, reference)?;
            mutate(root, &["merge", "--no-edit", "--", reference])
        }
        RefAction::Update if local.is_some() => {
            let branch = local.unwrap();
            let (server, target) = upstream(root, reference)?;
            let from = format!("{LOCAL}{target}");
            if head.as_deref() == Some(reference) {
                network(
                    root,
                    &server,
                    false,
                    &[
                        "pull",
                        "--ff-only",
                        "--no-rebase",
                        "--no-recurse-submodules",
                        "--",
                        &server,
                        &from,
                    ],
                )
            } else {
                if worktree_of_branch(root, branch).is_some() {
                    return Err(err("This branch is checked out in another worktree"));
                }
                let refspec = format!("{from}:{reference}");
                network(
                    root,
                    &server,
                    false,
                    &[
                        "fetch",
                        "--no-tags",
                        "--no-recurse-submodules",
                        "--",
                        &server,
                        &refspec,
                    ],
                )
            }
        }
        RefAction::Push {
            remote,
            target_branch,
        } if local.is_some() => {
            branch_name(root, &target_branch)?;
            let refspec = format!("{reference}:{LOCAL}{target_branch}");
            network(
                root,
                &remote,
                true,
                &[
                    "push",
                    "--no-follow-tags",
                    "--no-recurse-submodules",
                    "-u",
                    "--",
                    &remote,
                    &refspec,
                ],
            )
        }
        RefAction::Rename { new_branch } if local.is_some() => {
            branch_name(root, &new_branch)?;
            mutate(root, &["branch", "-m", "--", local.unwrap(), &new_branch])
        }
        RefAction::Delete => {
            if let Some(branch) = local {
                mutate(root, &["branch", "-d", "--", branch])
            } else if let Some(name) = remote {
                let (server, branch) = remote_parts(root, name)?;
                let destination = format!("{LOCAL}{branch}");
                // 拒绝删除尚未在列表中看到的远端新提交，不以强推掩盖远端变化。
                let lease = format!("--force-with-lease={destination}:{}", oid.trim_end());
                network(
                    root,
                    &server,
                    true,
                    &[
                        "push",
                        "--no-follow-tags",
                        "--no-recurse-submodules",
                        &lease,
                        "--",
                        &server,
                        &format!(":{destination}"),
                    ],
                )
            } else {
                mutate(root, &["tag", "-d", "--", tag.unwrap()])
            }
        }
        RefAction::PushTag if tag.is_some() => network(
            root,
            "origin",
            true,
            &[
                "push",
                "--no-follow-tags",
                "--no-recurse-submodules",
                "--",
                "origin",
                reference,
            ],
        ),
        _ => Err(err("Action is not available for this reference")),
    }
}
