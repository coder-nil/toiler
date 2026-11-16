import { execFile } from "child_process";
import { chmodSync, existsSync, mkdtempSync, realpathSync, rmSync, writeFileSync } from "fs";
import { tmpdir } from "os";
import path from "path";
import { join } from "path";
import { promisify } from "util";
import { getGitStatus, listUntrackedPaths } from "./git-changes";
import { gitCredentialKind, loadGitCredential, saveGitCredential, type GitCredential } from "./git-credentials";

const execFileAsync = promisify(execFile);
const GIT_TIMEOUT_MS = 60_000;
const GIT_MAX_BUFFER = 8 * 1024 * 1024;
const MAX_STAGED_DIFF_CHARS = 60_000;

export type GitOperationKind = "merge" | "rebase" | "cherry-pick" | "revert" | null;
export type GitAction =
  | "stage" | "unstage" | "discard" | "discard_all" | "commit" | "fetch" | "pull" | "push" | "merge" | "continue" | "abort"
  | "create_branch" | "rename_branch" | "delete_branch" | "checkout_remote_branch" | "delete_remote_branch" | "set_remote_url"
  | "pull_branch" | "push_branch" | "merge_branch";

export interface GitSummary {
  isGitRepository: boolean;
  repositoryRoot: string | null;
  branch: string | null;
  upstream: string | null;
  ahead: number;
  behind: number;
  remote: string | null;
  credentialKind: ReturnType<typeof gitCredentialKind>;
  hasSavedCredential: boolean;
  savedCredentialUsername: string | null;
  canRememberCredential: boolean;
  operation: GitOperationKind;
  branches: string[];
  changes: Awaited<ReturnType<typeof getGitStatus>>;
  /**
   * Untracked files that `discard_all` will delete; nothing can restore them.
   *
   * `getGitStatus` cannot supply this list: it returns worktree-relative paths
   * and therefore reports nothing at all for a cwd inside a subdirectory.
   */
  untrackedPaths: string[];
}

declare global {
  var __piGitLocks: Map<string, Promise<unknown>> | undefined;
}

function locks(): Map<string, Promise<unknown>> {
  if (!globalThis.__piGitLocks) globalThis.__piGitLocks = new Map();
  return globalThis.__piGitLocks;
}

async function git(cwd: string, args: string[], env: Partial<NodeJS.ProcessEnv> = {}): Promise<string> {
  const { stdout } = await execFileAsync("git", ["-C", cwd, ...args], {
    timeout: GIT_TIMEOUT_MS,
    maxBuffer: GIT_MAX_BUFFER,
    // Never leave a web request blocked on a terminal editor or credentials prompt.
    env: { ...process.env, LC_ALL: "C", GIT_TERMINAL_PROMPT: "0", GIT_EDITOR: "true", ...env },
  });
  return stdout.trim();
}

/**
 * Credentials are keyed by the remote URL, so a branch-level remote operation
 * has to look up the credential of *its* remote rather than assuming `origin`:
 * `pull_branch`/`push_branch`/`delete_remote_branch` must not skip the askpass
 * broker, or a passphrase-protected SSH key fails with
 * "Permission denied (publickey)" even when the user saved that passphrase
 * from the panel.
 */
async function remoteUrl(cwd: string, name = "origin"): Promise<string | null> {
  return (await git(cwd, ["remote", "get-url", name]).catch(() => "")) || null;
}

async function gitWithCredential(cwd: string, args: string[], remote: string | null, supplied: GitCredential | null): Promise<string> {
  const credential = supplied ?? await loadGitCredential(remote);
  if (!credential) return git(cwd, args);
  const temporaryDirectory = mkdtempSync(join(tmpdir(), "pi-git-askpass-"));
  const secretPath = join(temporaryDirectory, "secret");
  const usernamePath = join(temporaryDirectory, "username");
  const askpassPath = join(temporaryDirectory, process.platform === "win32" ? "askpass.cmd" : "askpass.sh");
  try {
    writeFileSync(secretPath, credential.secret, { encoding: "utf8", mode: 0o600 });
    writeFileSync(usernamePath, credential.username ?? "git", { encoding: "utf8", mode: 0o600 });
    const script = process.platform === "win32"
      ? "@echo off\r\n echo %~1 | findstr /I /C:\"username\" >nul && type \"%PI_GIT_USERNAME_FILE%\" || type \"%PI_GIT_SECRET_FILE%\"\r\n"
      : "#!/bin/sh\ncase \"$1\" in *[Uu]sername*) cat \"$PI_GIT_USERNAME_FILE\" ;; *) cat \"$PI_GIT_SECRET_FILE\" ;; esac\n";
    writeFileSync(askpassPath, script, { encoding: "utf8", mode: 0o700 });
    chmodSync(askpassPath, 0o700);
    return await git(cwd, args, {
      GIT_ASKPASS: askpassPath,
      SSH_ASKPASS: askpassPath,
      SSH_ASKPASS_REQUIRE: "force",
      DISPLAY: process.env.DISPLAY || "pi-desktop",
      PI_GIT_SECRET_FILE: secretPath,
      PI_GIT_USERNAME_FILE: usernamePath,
    });
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true, maxRetries: 3 });
  }
}

function errorMessage(error: unknown): string {
  const stderr = (error as { stderr?: string }).stderr;
  if (typeof stderr === "string" && stderr.trim()) return stderr.trim();
  return error instanceof Error ? error.message : String(error);
}

async function repositoryRoot(cwd: string): Promise<string | null> {
  try {
    return (await git(cwd, ["rev-parse", "--show-toplevel"])).trim() || null;
  } catch {
    return null;
  }
}

/** Returns the staged patch for AI-assisted commit-message generation. */
export async function getStagedDiff(cwd: string): Promise<string> {
  if (!await repositoryRoot(cwd)) throw new Error("Not a Git repository");
  const diff = await git(cwd, ["diff", "--cached", "--no-ext-diff", "--no-color", "--unified=3", "--"]);
  if (!diff.trim()) throw new Error("No staged changes to summarize");
  return diff.length > MAX_STAGED_DIFF_CHARS
    ? `${diff.slice(0, MAX_STAGED_DIFF_CHARS)}\n\n[Diff truncated]`
    : diff;
}

function operationFor(repositoryRoot: string): GitOperationKind {
  const gitDir = path.join(repositoryRoot, ".git");
  // In linked worktrees .git is a file, therefore ask Git for its real directory
  // in getGitSummary/runGitAction instead of resolving this path directly.
  if (existsSync(path.join(gitDir, "MERGE_HEAD"))) return "merge";
  if (existsSync(path.join(gitDir, "CHERRY_PICK_HEAD"))) return "cherry-pick";
  if (existsSync(path.join(gitDir, "REVERT_HEAD"))) return "revert";
  if (existsSync(path.join(gitDir, "rebase-merge")) || existsSync(path.join(gitDir, "rebase-apply"))) return "rebase";
  return null;
}

async function currentOperation(cwd: string, root: string): Promise<GitOperationKind> {
  try {
    const gitDir = await git(cwd, ["rev-parse", "--absolute-git-dir"]);
    if (existsSync(path.join(gitDir, "MERGE_HEAD"))) return "merge";
    if (existsSync(path.join(gitDir, "CHERRY_PICK_HEAD"))) return "cherry-pick";
    if (existsSync(path.join(gitDir, "REVERT_HEAD"))) return "revert";
    if (existsSync(path.join(gitDir, "rebase-merge")) || existsSync(path.join(gitDir, "rebase-apply"))) return "rebase";
  } catch {
    return operationFor(root);
  }
  return null;
}

async function withRepositoryLock<T>(cwd: string, work: (root: string) => Promise<T>): Promise<T> {
  const root = await repositoryRoot(cwd);
  if (!root) throw new Error("Not a Git repository");
  const map = locks();
  const previous = map.get(root) ?? Promise.resolve();
  const next = previous.catch(() => undefined).then(() => work(root));
  map.set(root, next);
  try {
    return await next;
  } finally {
    if (map.get(root) === next) map.delete(root);
  }
}

function assertPaths(paths: unknown): asserts paths is string[] {
  if (!Array.isArray(paths) || paths.length === 0 || paths.some((p) => typeof p !== "string" || !p.trim())) {
    throw new Error("At least one file path is required");
  }
}

function canonicalPath(filePath: string): string {
  const resolved = path.resolve(filePath);
  try {
    return realpathSync.native(resolved);
  } catch {
    try {
      return path.join(realpathSync.native(path.dirname(resolved)), path.basename(resolved));
    } catch {
      return resolved;
    }
  }
}

function relativePaths(root: string, paths: string[]): string[] {
  const canonicalRoot = canonicalPath(root);
  return paths.map((filePath) => {
    const relative = path.relative(canonicalRoot, canonicalPath(filePath));
    if (!relative || relative === ".." || relative.startsWith(`..${path.sep}`) || path.isAbsolute(relative)) {
      throw new Error("A requested file is outside this repository");
    }
    return relative;
  });
}

async function assertBranchName(cwd: string, branch: unknown, label = "Branch"): Promise<string> {
  if (typeof branch !== "string" || !branch.trim()) throw new Error(`${label} is required`);
  const trimmed = branch.trim();
  try {
    await git(cwd, ["check-ref-format", "--branch", trimmed]);
  } catch {
    throw new Error(`Invalid branch name: ${trimmed}`);
  }
  return trimmed;
}

async function assertRemoteUrl(remoteUrl: unknown): Promise<string> {
  if (typeof remoteUrl !== "string" || !remoteUrl.trim()) throw new Error("A remote URL is required");
  const trimmed = remoteUrl.trim();
  if (/[\0\r\n]/.test(trimmed)) throw new Error("Invalid remote URL");
  return trimmed;
}

async function remoteBranchRef(cwd: string, remoteBranch: unknown): Promise<{ remote: string; branch: string; ref: string }> {
  if (typeof remoteBranch !== "string") throw new Error("Remote branch is required");
  const ref = remoteBranch.trim();
  const separator = ref.indexOf("/");
  if (separator <= 0 || separator === ref.length - 1) throw new Error("Invalid remote branch");
  const remote = ref.slice(0, separator);
  const branch = await assertBranchName(cwd, ref.slice(separator + 1));
  try {
    await git(cwd, ["rev-parse", "--verify", `refs/remotes/${remote}/${branch}`]);
  } catch {
    throw new Error(`Remote branch not found: ${ref}`);
  }
  return { remote, branch, ref };
}

async function upstreamForBranch(cwd: string, branch: unknown): Promise<{ branch: string; remote: string; remoteBranch: string }> {
  const localBranch = await assertBranchName(cwd, branch);
  let upstream: string;
  try {
    upstream = await git(cwd, ["rev-parse", "--abbrev-ref", "--symbolic-full-name", `${localBranch}@{upstream}`]);
  } catch {
    throw new Error(`Branch ${localBranch} has no upstream`);
  }
  const separator = upstream.indexOf("/");
  if (separator <= 0 || separator === upstream.length - 1) throw new Error(`Invalid upstream for ${localBranch}`);
  return { branch: localBranch, remote: upstream.slice(0, separator), remoteBranch: upstream.slice(separator + 1) };
}

async function worktreeForBranch(cwd: string, branch: string): Promise<string | null> {
  const out = await git(cwd, ["worktree", "list", "--porcelain"]);
  let worktreePath: string | null = null;
  for (const line of out.split("\n")) {
    if (line.startsWith("worktree ")) worktreePath = line.slice("worktree ".length).trim();
    else if (line === `branch refs/heads/${branch}` && worktreePath) return worktreePath;
    else if (!line.trim()) worktreePath = null;
  }
  return null;
}

export async function getGitSummary(cwd: string): Promise<GitSummary> {
  const root = await repositoryRoot(cwd);
  if (!root) {
    return { isGitRepository: false, repositoryRoot: null, branch: null, upstream: null, ahead: 0, behind: 0, remote: null, credentialKind: "none", hasSavedCredential: false, savedCredentialUsername: null, canRememberCredential: false, operation: null, branches: [], changes: await getGitStatus(cwd), untrackedPaths: [] };
  }
  const [branchResult, upstreamResult, remoteResult, branchesResult, changes, operation] = await Promise.all([
    git(cwd, ["branch", "--show-current"]).catch(() => ""),
    git(cwd, ["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{upstream}"]).catch(() => ""),
    git(cwd, ["remote", "get-url", "origin"]).catch(() => ""),
    git(cwd, ["for-each-ref", "--format=%(refname:short)", "refs/heads"]).catch(() => ""),
    getGitStatus(cwd),
    currentOperation(cwd, root),
  ]);
  const upstream = upstreamResult || null;
  let ahead = 0;
  let behind = 0;
  if (upstream) {
    try {
      // `HEAD...upstream` prints "left right": commits only in HEAD (ahead)
      // followed by commits only in upstream (behind).
      const [aheadText, behindText] = (await git(cwd, ["rev-list", "--left-right", "--count", `HEAD...${upstream}`])).split(/\s+/);
      ahead = Number(aheadText) || 0;
      behind = Number(behindText) || 0;
    } catch { /* upstream may be unavailable locally */ }
  }
  const branches = [...new Set(branchesResult.split("\n").map((branch) => branch.trim()).filter(Boolean))]
    .sort((a, b) => a.localeCompare(b));
  const remote = remoteResult || null;
  let savedCredential: GitCredential | null = null;
  try { savedCredential = await loadGitCredential(remote); } catch { /* Keychain may be unavailable. */ }
  const hasSavedCredential = Boolean(savedCredential);
  // The username is not a secret (the token is) — echoing it back lets the panel
  // prefill the field so users can see what they previously saved.
  const savedCredentialUsername = savedCredential?.kind === "https" ? savedCredential.username ?? null : null;
  return { isGitRepository: true, repositoryRoot: root, branch: branchResult || null, upstream, ahead, behind, remote, credentialKind: gitCredentialKind(remote), hasSavedCredential, savedCredentialUsername, canRememberCredential: process.platform === "darwin", operation, branches, changes, untrackedPaths: await listUntrackedPaths(root) };
}

export async function runGitAction(cwd: string, action: GitAction, input: { paths?: unknown; message?: unknown; rebase?: unknown; branch?: unknown; newBranch?: unknown; startPoint?: unknown; targetBranch?: unknown; remoteUrl?: unknown; credential?: GitCredential; rememberCredential?: unknown }): Promise<GitSummary> {
  await withRepositoryLock(cwd, async (root) => {
    try {
      if (action === "stage" || action === "unstage" || action === "discard") {
        assertPaths(input.paths);
        const paths = relativePaths(root, input.paths);
        if (action === "stage") await git(cwd, ["add", "--", ...paths]);
        if (action === "unstage") await git(cwd, ["restore", "--staged", "--", ...paths]);
        if (action === "discard") {
          // Unstage first so a newly-added file becomes an ordinary untracked
          // path. Only paths that already exist in HEAD can be restored from
          // HEAD; the rest must be removed with clean.
          await git(cwd, ["restore", "--staged", "--", ...paths]);
          const headPaths = await git(cwd, ["ls-tree", "-r", "--name-only", "HEAD", "--", ...paths]).catch(() => "");
          const pathsInHead = headPaths.split("\n").filter(Boolean);
          if (pathsInHead.length > 0) await git(cwd, ["restore", "--worktree", "--source=HEAD", "--", ...pathsInHead]);
          await git(cwd, ["clean", "-f", "--quiet", "--", ...paths]);
        }
      } else if (action === "discard_all") {
        // "Discard all" restores the whole checkout to its pre-edit state, so it
        // has to cover all three kinds of local change:
        //   1. staged changes in the index          -> reset --mixed
        //   2. unstaged/deleted tracked files       -> checkout -- .
        //   3. untracked files that only exist here -> clean -fd
        // The default `git clean -fd` (no -x) keeps ignored files, so build
        // output and .env-style local config survive. `reset` and `checkout`
        // need a commit to fall back to; a repository without HEAD can still
        // drop its staged and untracked changes.
        const hasHead = !(await git(cwd, ["rev-parse", "--verify", "--quiet", "HEAD"]).then(() => false, () => true));
        if (hasHead) await git(cwd, ["reset", "--quiet"]);
        if (hasHead) await git(cwd, ["checkout", "--", "."]);
        await git(cwd, ["clean", "-fd", "--quiet"]);
      } else if (action === "commit") {
        if (typeof input.message !== "string" || !input.message.trim()) throw new Error("A commit message is required");
        await git(cwd, ["commit", "-m", input.message.trim()]);
      } else if (action === "fetch" || action === "pull" || action === "push") {
        const remote = await remoteUrl(cwd);
        const kind = gitCredentialKind(remote);
        const credential = input.credential;
        if (credential && credential.kind !== kind) throw new Error("Credential type does not match the remote URL");
        if (credential?.kind === "https" && !credential.username?.trim()) throw new Error("A username is required for an HTTPS remote");
        const args = action === "fetch" ? ["fetch", "--prune"] : action === "pull" ? ["pull", input.rebase === true ? "--rebase" : "--no-rebase"] : ["push"];
        await gitWithCredential(cwd, args, remote, credential ?? null);
        if (credential && input.rememberCredential === true && remote) await saveGitCredential(remote, credential);
      } else if (action === "set_remote_url") {
        await git(cwd, ["remote", "set-url", "origin", "--", await assertRemoteUrl(input.remoteUrl)]);
      } else if (action === "merge") {
        if (typeof input.branch !== "string" || !input.branch.trim()) throw new Error("A branch is required");
        await git(cwd, ["merge", "--no-edit", input.branch.trim()]);
      } else if (action === "create_branch") {
        const branch = await assertBranchName(cwd, input.branch);
        if (input.startPoint === undefined) {
          await git(cwd, ["branch", "--", branch]);
        } else {
          const remote = await remoteBranchRef(cwd, input.startPoint);
          await git(cwd, ["branch", "--", branch, `refs/remotes/${remote.ref}`]);
        }
        await git(cwd, ["checkout", "--quiet", branch]);
      } else if (action === "rename_branch") {
        const branch = await assertBranchName(cwd, input.branch, "Current branch");
        const newBranch = await assertBranchName(cwd, input.newBranch, "New branch name");
        await git(cwd, ["branch", "-m", "--", branch, newBranch]);
      } else if (action === "delete_branch") {
        // The UI has already asked for confirmation. Use Git's force form so
        // an unmerged branch is removed instead of appearing unchanged.
        await git(cwd, ["branch", "-D", "--", await assertBranchName(cwd, input.branch)]);
      } else if (action === "checkout_remote_branch") {
        const remote = await remoteBranchRef(cwd, input.branch);
        try {
          await git(cwd, ["rev-parse", "--verify", `refs/heads/${remote.branch}`]);
          await git(cwd, ["checkout", "--quiet", remote.branch]);
        } catch {
          await git(cwd, ["checkout", "--quiet", "--track", "-b", remote.branch, remote.ref]);
        }
      } else if (action === "delete_remote_branch") {
        const remote = await remoteBranchRef(cwd, input.branch);
        const url = await remoteUrl(cwd, remote.remote);
        await gitWithCredential(cwd, ["push", remote.remote, "--delete", remote.branch], url, null);
        // `push --delete` does not necessarily remove the local tracking ref,
        // which would otherwise leave the deleted branch visible in the menu.
        await gitWithCredential(cwd, ["fetch", "--prune", remote.remote], url, null);
      } else if (action === "pull_branch") {
        const upstream = await upstreamForBranch(cwd, input.branch);
        await gitWithCredential(cwd, ["fetch", upstream.remote, `${upstream.remoteBranch}:refs/heads/${upstream.branch}`], await remoteUrl(cwd, upstream.remote), null);
      } else if (action === "push_branch") {
        const upstream = await upstreamForBranch(cwd, input.branch);
        await gitWithCredential(cwd, ["push", upstream.remote, `refs/heads/${upstream.branch}:refs/heads/${upstream.remoteBranch}`], await remoteUrl(cwd, upstream.remote), null);
      } else if (action === "merge_branch") {
        const source = await assertBranchName(cwd, input.branch, "Source branch");
        const target = await assertBranchName(cwd, input.targetBranch, "Target branch");
        if (source === target) throw new Error("Source and target branches must be different");
        await git(cwd, ["rev-parse", "--verify", `refs/heads/${source}`]);
        await git(cwd, ["rev-parse", "--verify", `refs/heads/${target}`]);
        const targetCwd = await worktreeForBranch(cwd, target);
        if (targetCwd) {
          await git(targetCwd, ["merge", "--no-edit", source]);
        } else {
          await git(cwd, ["checkout", "--quiet", target]);
          await git(cwd, ["merge", "--no-edit", source]);
        }
      } else {
        const operation = await currentOperation(cwd, root);
        if (!operation) throw new Error("No Git operation is currently in progress");
        const command = action === "continue" ? "--continue" : "--abort";
        await git(cwd, [operation, command]);
      }
    } catch (error) {
      throw new Error(errorMessage(error));
    }
  });
  const summary = await getGitSummary(cwd);
  return { ...summary, untrackedPaths: summary.isGitRepository ? await listUntrackedPaths(summary.repositoryRoot ?? cwd) : [] };
}
