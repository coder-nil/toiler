export function parseFileRenameName(value: unknown): string | null {
  if (typeof value !== "string") return null;
  const name = value.trim();
  if (!name || name === "." || name === "..") return null;
  if (name.includes("/") || name.includes("\\") || name.includes("\0")) return null;
  return name;
}
