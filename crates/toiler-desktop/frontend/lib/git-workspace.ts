export type GitDiffSelection =
  | { scope: "unstaged" | "staged"; path: string }
  | { scope: "commit"; hash: string; path: string; oldPath: string | null };

export function workspaceDiffUrl(cwd: string, selection: GitDiffSelection) {
  const query = new URLSearchParams({ cwd, scope: selection.scope, path: selection.path });
  if (selection.scope === "commit") {
    query.set("hash", selection.hash);
    if (selection.oldPath) query.set("oldPath", selection.oldPath);
  }
  return `/api/git/workspace-diff?${query}`;
}
