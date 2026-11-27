export interface GitHistoryRef {
  name: string;
  commit: string | null;
}

export interface GitHistoryRefs {
  repositoryRoot: string;
  headBranch: string | null;
  refs: GitHistoryRef[];
  remotes: string[];
  upstreams: Record<string, { remote: string; branch: string }>;
}

export const REF_ACTIONS = {
  local: ["checkout", "create_branch", "merge", "update", "push", "rename", "delete"],
  remote: ["checkout", "create_branch", "merge", "delete"],
  tag: ["checkout", "merge", "push_tag", "delete"],
} as const;

export type RefKind = keyof typeof REF_ACTIONS;
export type RefAction = (typeof REF_ACTIONS)[RefKind][number];

export function historyRefKind(reference: string): RefKind {
  if (reference.startsWith("refs/tags/")) return "tag";
  return reference.startsWith("refs/remotes/") ? "remote" : "local";
}

export interface GitHistorySummary {
  hash: string;
  parents: string[];
  subject: string;
  author: string;
  isOwnAuthor: boolean;
  authoredAt: string;
}

export interface GitHistoryPage {
  start: string | null;
  commits: GitHistorySummary[];
  nextOffset: number | null;
}

export interface GitHistoryBranch {
  name: string;
  isTip: boolean;
}

export interface GitHistoryCommit extends GitHistorySummary {
  authorEmail: string;
  message: string;
  files: { status: string; path: string; oldPath: string | null }[];
  containingBranches: GitHistoryBranch[];
}

export interface GitHistoryCriteria {
  commitId: string;
  subject: string;
  author: string;
  fromDate: string;
  toDate: string;
}

export function historyFilterParams(criteria: GitHistoryCriteria): Record<string, string> {
  const { commitId, subject, author, fromDate, toDate } = criteria;
  const params: Record<string, string> = {};
  for (const [key, value] of Object.entries({ commitId: commitId.toLowerCase(), subject, author })) {
    if (value.trim()) params[key] = value.trim();
  }
  if (fromDate) params.from = String(localDayBoundary(fromDate));
  if (toDate) params.before = String(localDayBoundary(toDate, 1));
  return params;
}

function localDayBoundary(value: string, days = 0): number {
  const date = new Date(`${value}T00:00:00`);
  // 下一天的本地零点包含完整的结束日，也适用于夏令时切换日。
  date.setDate(date.getDate() + days);
  return date.getTime() / 1000;
}

export function shortRef(name: string): string {
  return name.replace(/^refs\/(heads|remotes|tags)\//, "");
}

interface GraphRow {
  lane: number;
  enters: boolean;
  through: number[];
  parents: number[];
}

export function layoutGitHistory(commits: Pick<GitHistorySummary, "hash" | "parents">[]) {
  const lanes: (string | null)[] = [];
  const pending = new Map<string, number>();
  const occupy = (hash: string, preferred?: number) => {
    const existing = pending.get(hash);
    if (existing !== undefined) return existing;
    const gap = lanes.indexOf(null);
    const lane = preferred ?? (gap < 0 ? lanes.length : gap);
    pending.set(hash, lane);
    lanes[lane] = hash;
    return lane;
  };
  let width = 1;
  const rows: GraphRow[] = commits.map(({ hash, parents }) => {
    const enters = pending.has(hash);
    const lane = occupy(hash);
    const through = Array.from(pending.values()).filter((slot) => slot !== lane);
    pending.delete(hash);
    // 其他活动线不整体左移，翻页也不重新分配旧行的线槽。
    lanes[lane] = null;
    const parentLanes = parents.map((parent, index) => occupy(parent, index === 0 ? lane : undefined));
    width = Math.max(width, lanes.length);
    return { lane, enters, through, parents: parentLanes };
  });
  return { width, rows };
}
