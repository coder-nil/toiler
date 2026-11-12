function safeDecode(value: string): string {
  try {
    return decodeURIComponent(value);
  } catch {
    return value;
  }
}

function normalizeFilePathSlashes(filePath: string): string {
  if (/^[a-zA-Z]:[\\/]/.test(filePath) || filePath.startsWith("\\\\")) {
    return filePath.replace(/\\/g, "/");
  }
  return filePath;
}

/** Where in a file a link points, when it carried `:12`, `:12:5` or `#L12`. */
export interface FileOpenLocation {
  /** 1-based line number. */
  line?: number;
  /** 1-based column, when the link carried one. */
  column?: number;
}

/** A resolved local file link: the path plus the optional source location. */
export interface FileLinkTarget extends FileOpenLocation {
  filePath: string;
}

function parseLocationSuffix(filePath: string): { filePath: string } & FileOpenLocation {
  const match = filePath.match(/^(.*?):(\d+)(?::(\d+))?$/);
  if (!match || !match[1]) return { filePath };
  return {
    filePath: match[1],
    line: Number(match[2]),
    ...(match[3] ? { column: Number(match[3]) } : {}),
  };
}

/** GitHub-style fragments: `#L42`, `#L42C7`, `#L42-L60`, `#L42C7-L60`. */
function parseHashLocation(hash: string): FileOpenLocation {
  const match = hash.match(/^#l(\d+)(?:c(\d+))?(?:-l?(\d+)(?:c(\d+))?)?$/i);
  if (!match) return {};
  return {
    line: Number(match[1]),
    ...(match[2] ? { column: Number(match[2]) } : {}),
  };
}

function normalizeLocalPath(filePath: string): string {
  const normalized = normalizeFilePathSlashes(filePath);
  const isWindowsDrive = /^[a-zA-Z]:\//.test(normalized);
  const isUnc = normalized.startsWith("//");
  const leadingSlash = normalized.startsWith("/") && !isWindowsDrive && !isUnc;
  const parts: string[] = [];

  for (const part of normalized.split("/")) {
    if (!part || part === ".") continue;
    if (part === "..") {
      if (parts.length > 0 && parts[parts.length - 1] !== "..") {
        parts.pop();
      } else if (!leadingSlash && !isWindowsDrive && !isUnc) {
        parts.push(part);
      }
      continue;
    }
    parts.push(part);
  }

  const joined = parts.join("/");
  if (isWindowsDrive) return joined;
  if (isUnc) return `//${joined}`;
  return leadingSlash ? `/${joined}` : joined;
}

function isPathInside(candidate: string, root: string): boolean {
  const normalizedCandidate = normalizeLocalPath(candidate).replace(/\/+$/, "");
  const normalizedRoot = normalizeLocalPath(root).replace(/\/+$/, "");
  const useCaseInsensitive = /^[a-zA-Z]:\//.test(normalizedCandidate) || /^[a-zA-Z]:\//.test(normalizedRoot);
  const filePath = useCaseInsensitive ? normalizedCandidate.toLowerCase() : normalizedCandidate;
  const rootPath = useCaseInsensitive ? normalizedRoot.toLowerCase() : normalizedRoot;
  return filePath === rootPath || filePath.startsWith(`${rootPath}/`);
}

function looksLikeRelativeFileHref(href: string): boolean {
  if (href.startsWith("#") || href.startsWith("?")) return false;
  if (href.startsWith("./") || href.startsWith("../")) return true;
  if (href.includes("/")) return true;
  return /(^|\/)\.?[^/]+\.[^/.]+$/.test(href);
}

function fileUrlToPath(href: string): string | null {
  try {
    const url = new URL(href);
    if (url.protocol !== "file:") return null;
    const pathname = safeDecode(url.pathname);
    if (url.hostname) {
      return `//${url.hostname}${pathname.startsWith("/") ? pathname : `/${pathname}`}`;
    }
    if (/^\/[a-zA-Z]:\//.test(pathname)) return pathname.slice(1);
    return pathname;
  } catch {
    return null;
  }
}

export function resolveLocalFileTarget(
  href: string | undefined,
  baseDir?: string,
  relativeRoot = baseDir,
): FileLinkTarget | null {
  if (!href) return null;

  const trimmedHref = href.trim();
  const hashIndex = trimmedHref.indexOf("#");
  const hashLocation = hashIndex >= 0 ? parseHashLocation(trimmedHref.slice(hashIndex)) : {};
  const cleanHref = (hashIndex >= 0 ? trimmedHref.slice(0, hashIndex) : trimmedHref)
    .split("?", 1)[0]
    .trim();
  if (!cleanHref) return null;

  let candidate: string | null = null;
  let candidateKind: "absolute" | "relative" | null = null;
  const decodedHref = safeDecode(cleanHref);
  const isBackslashUncPath = decodedHref.startsWith("\\\\");
  const normalizedHref = normalizeFilePathSlashes(decodedHref);
  const lowerHref = normalizedHref.toLowerCase();

  if (lowerHref.startsWith("/api/") || lowerHref.startsWith("/_next/")) return null;
  if (!isBackslashUncPath && normalizedHref.startsWith("//")) return null;
  if (/^[a-zA-Z][a-zA-Z0-9+.-]*:/i.test(normalizedHref) && !lowerHref.startsWith("file:") && !/^[a-zA-Z]:\//.test(normalizedHref)) {
    return null;
  }

  if (lowerHref.startsWith("file:")) {
    candidate = fileUrlToPath(normalizedHref);
    candidateKind = candidate ? "absolute" : null;
  } else if (/^[a-zA-Z]:\//.test(normalizedHref)) {
    candidate = normalizedHref;
    candidateKind = "absolute";
  } else if (normalizedHref.startsWith("/")) {
    candidate = normalizedHref;
    candidateKind = "absolute";
  } else if (baseDir && looksLikeRelativeFileHref(normalizedHref)) {
    candidate = `${normalizeFilePathSlashes(baseDir).replace(/\/+$/, "")}/${normalizedHref}`;
    candidateKind = "relative";
  }

  if (!candidate) return null;

  const located = parseLocationSuffix(normalizeLocalPath(candidate));
  const filePath = located.filePath;
  if (candidateKind === "relative" && relativeRoot && !isPathInside(filePath, relativeRoot)) return null;
  return {
    filePath,
    ...(located.line ?? hashLocation.line
      ? { line: located.line ?? hashLocation.line }
      : {}),
    ...(located.column ?? hashLocation.column
      ? { column: located.column ?? hashLocation.column }
      : {}),
  };
}

/** Path-only view of {@link resolveLocalFileTarget}, for callers that ignore line numbers. */
export function resolveLocalFileHref(
  href: string | undefined,
  baseDir?: string,
  relativeRoot = baseDir,
): string | null {
  return resolveLocalFileTarget(href, baseDir, relativeRoot)?.filePath ?? null;
}

/** Resolve a filesystem path without applying URL or source-location syntax. */
export function resolveLocalFilePath(filePath: string | undefined, baseDir?: string): string | null {
  if (!filePath) return null;

  const windowsStyle = /^[a-zA-Z]:[\\/]/.test(filePath) ||
    filePath.startsWith("\\\\") ||
    (baseDir !== undefined && (/^[a-zA-Z]:[\\/]/.test(baseDir) || baseDir.startsWith("\\\\")));
  const normalizeSlashes = (value: string) => windowsStyle ? value.replace(/\\/g, "/") : value;
  const normalizedPath = normalizeSlashes(filePath);
  const normalizedBase = baseDir ? normalizeSlashes(baseDir).replace(/\/+$/, "") : undefined;

  const isDriveAbsolute = /^[a-zA-Z]:\//.test(normalizedPath);
  const isUncAbsolute = normalizedPath.startsWith("//");
  let candidate: string;

  if (isDriveAbsolute || isUncAbsolute) {
    candidate = normalizedPath;
  } else if (normalizedPath.startsWith("/")) {
    const windowsRoot = normalizedBase?.match(/^([a-zA-Z]:)(?:\/|$)/)?.[1]
      ?? normalizedBase?.match(/^(\/\/[^/]+\/[^/]+)(?:\/|$)/)?.[1];
    candidate = windowsRoot ? `${windowsRoot}${normalizedPath}` : normalizedPath;
  } else {
    if (!normalizedBase) return null;
    candidate = `${normalizedBase}/${normalizedPath}`;
  }

  return normalizeLocalPath(candidate);
}
