function normalizePath(value: string, windows: boolean): string {
  const path = windows ? value.replace(/\\/g, "/") : value;
  const prefix = path.startsWith("//") && windows ? "//" : path.startsWith("/") ? "/" : "";
  const drive = windows && /^[a-z]:\//i.test(path);
  const rooted = prefix !== "" || drive;
  const rootSegments = prefix === "//" ? 2 : drive ? 1 : 0;
  const segments: string[] = [];
  for (const part of path.slice(prefix.length).split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (segments.length > rootSegments && segments.at(-1) !== "..") segments.pop();
      else if (!rooted) segments.push(part);
    } else {
      segments.push(part);
    }
  }
  const normalized = prefix + segments.join("/") || ".";
  return windows ? normalized.toLowerCase() : normalized;
}

/** 文件列表比较的是服务端路径，不能依赖浏览器运行平台或 Node 的 path 模块。 */
export function samePath(a: string, b: string): boolean {
  if (a === b) return true;
  if (!a || !b) return false;
  const windows = [a, b].some((path) => /^[a-z]:[\\/]/i.test(path)
    || path.startsWith("\\\\") || path.startsWith("//"));
  return normalizePath(a, windows) === normalizePath(b, windows);
}
