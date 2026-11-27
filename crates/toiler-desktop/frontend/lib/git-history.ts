export interface GitHistoryRef {
  name: string;
  commit: string | null;
}

export interface GitHistoryRefs {
  repositoryRoot: string;
  headBranch: string | null;
  refs: GitHistoryRef[];
}

export interface GitHistorySummary {
  hash: string;
  parents: string[];
  subject: string;
  author: string;
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
