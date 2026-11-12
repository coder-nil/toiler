"use client";

import { forwardRef, useState, useCallback, useEffect, useImperativeHandle, useMemo, useRef } from "react";
import { RevealFileButton } from "./RevealFileButton";
import { getFileIcon, FolderIcon } from "./FileIcons";
import {
  encodeFilePathForApi,
  getFileDirectory,
  getFileName,
  getRelativeFilePath,
  joinFilePath,
  normalizeFilePathSlashes,
} from "@/lib/file-paths";
import type { GitFileStatus, GitFileStatusKind, GitStatusResponse } from "@/lib/git-types";
import { useI18n } from "@/hooks/useI18n";
import { AtSign, GitBranch } from "lucide-react";
import { samePath } from "@/lib/paths";
type Translate = ReturnType<typeof useI18n>["t"];

interface FileEntry {
  name: string;
  isDir: boolean;
  size: number;
  modified: string;
  /**
   * 真实路径。Java 源码根下的包链会被服务端折叠成 `com.example.demo` 一行，
   * 此时 `name` 不再是目录名，路径只能由服务端给出。
   */
  entryPath?: string;
  /** 该目录自身就是一个 Git 仓库（直接含有 `.git`），可以单独打开 Git 面板。 */
  gitRepository?: boolean;
}

interface FileNode {
  name: string;
  fullPath: string;
  isDir: boolean;
  size: number;
  children?: FileNode[];
  loaded?: boolean;
  /** 这一行代表一条被折叠的 Java 包链，`fullPath` 是最内层目录。 */
  packageChain?: boolean;
  /** 见 `FileEntry.gitRepository`。 */
  gitRepository?: boolean;
}

interface Props {
  cwd: string;
  onOpenFile: (filePath: string, fileName: string, options?: OpenFileOptions) => void;
  onFileRenamed?: (oldPath: string, newPath: string, newName: string) => void;
  canRenamePath?: (filePath: string) => boolean;
  refreshKey?: number;
  onAtMention?: (relativePath: string, isDir: boolean) => void;
  onAtMentions?: (relativePaths: string[]) => void;
  /**
   * 点击目录行上的 Git 徽标：以该子项目仓库为根打开 Git 面板。
   * 一个项目目录里可以有多个各自带 `.git` 的子项目，所以面板的仓库根跟着这行走。
   */
  onOpenRepositoryGitPanel?: (repositoryPath: string) => void;
  onUploadBusyChange?: (busy: boolean) => void;
  /** 根目录列表正在请求，用于把侧边栏标题行里的折叠箭头换成转圈。 */
  onLoadingChange?: (loading: boolean) => void;
  changesCollapsed: boolean;
  onChangesCountChange?: (count: number) => void;
  hideDotDirs?: boolean;
  onHideDotDirsChange?: (hide: boolean) => void;
}

export interface FileExplorerHandle {
  openUploadPicker: () => void;
  setHideDotDirs: (updater: (prev: boolean) => boolean) => void;
  getHideDotDirs: () => boolean;
}

/**
 * 与树的折叠箭头同尺寸的加载指示。请求进行时直接把它画在箭头的位置上，
 * 而不是在行尾再塞一个图标：文件名那一行空间本来就紧，多一个图标会让名字抖动。
 */
export function TreeLoadingSpinner({ size = 10, label }: { size?: number; label: string }) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={2.4}
      strokeLinecap="round"
      role="status"
      aria-label={label}
      style={{ flexShrink: 0, animation: "spin 0.8s linear infinite" }}
    >
      <path d="M21 12a9 9 0 1 1-2.64-6.36" />
    </svg>
  );
}

type UploadPhase = "idle" | "checking" | "uploading";
type UploadConflictStrategy = "error" | "overwrite" | "skip";

interface UploadError {
  name: string;
  error: string;
}

interface UploadResponse {
  uploaded?: string[];
  skipped?: string[];
  errors?: UploadError[];
  conflicts?: string[];
  nonReplaceable?: string[];
  error?: string;
}

interface UploadSummary {
  uploaded: string[];
  skipped: string[];
  errors: UploadError[];
}

interface PendingConflict {
  files: File[];
  conflicts: string[];
  nonReplaceable: string[];
}

interface RenameResponse {
  path?: string;
  name?: string;
  error?: string;
  code?: string;
}

async function fetchEntries(dirPath: string, cwd: string, hideDotDirs = false): Promise<FileNode[]> {
  const encoded = encodeFilePathForApi(dirPath);
  const isRoot = samePath(dirPath, cwd);
  const query = hideDotDirs && isRoot ? "?type=list&hideDotDirs=true" : "?type=list";
  const res = await fetch(`/api/files/${encoded}${query}`);
  if (!res.ok) {
    let message = `Failed to load files (HTTP ${res.status})`;
    try {
      const data = await res.json() as { error?: string };
      if (data.error) message = data.error;
    } catch {
      // ignore non-JSON error bodies
    }
    throw new Error(message);
  }
  const data = await res.json() as { entries?: FileEntry[] };
  return (data.entries ?? []).map((e) => ({
    name: e.name,
    fullPath: e.entryPath ?? joinFilePath(dirPath, e.name),
    isDir: e.isDir,
    size: e.size,
    children: e.isDir ? [] : undefined,
    loaded: !e.isDir,
    ...(e.entryPath !== undefined ? { packageChain: true } : {}),
    ...(e.gitRepository ? { gitRepository: true } : {}),
  }));
}

async function fetchGitStatus(cwd: string): Promise<GitStatusResponse> {
  const params = new URLSearchParams({ cwd });
  const res = await fetch(`/api/git/status?${params.toString()}`);
  if (!res.ok) throw new Error(`Failed to load Git status (HTTP ${res.status})`);
  return res.json() as Promise<GitStatusResponse>;
}

const GIT_STATUS_KEYS: Record<GitFileStatusKind, string> = {
  modified: "files.modified",
  added: "files.added",
  deleted: "files.deleted",
  renamed: "files.renamed",
  untracked: "files.untracked",
  conflict: "files.conflict",
};

const GIT_STATUS_COLORS: Record<GitFileStatusKind, string> = {
  modified: "#d6a84b",
  added: "#4ade80",
  deleted: "#f87171",
  renamed: "#60a5fa",
  untracked: "#4ade80",
  conflict: "#f87171",
};

function GitStatusBadge({ status, t }: { status: GitFileStatus; t: Translate }) {
  return (
    <span
      title={t(GIT_STATUS_KEYS[status.status])}
      aria-label={t(GIT_STATUS_KEYS[status.status])}
      style={{
        width: 14,
        height: 14,
        flexShrink: 0,
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        color: GIT_STATUS_COLORS[status.status],
        fontFamily: "var(--font-mono)",
        fontSize: 11,
        fontWeight: 600,
      }}
    >
      {status.code}
    </span>
  );
}

function uploadFiles(
  targetDirectory: string,
  files: File[],
  strategy: UploadConflictStrategy,
  onProgress: (progress: number) => void,
): Promise<{ status: number; data: UploadResponse }> {
  return new Promise((resolve, reject) => {
    const formData = new FormData();
    files.forEach((file) => formData.append("files", file, file.name));

    const xhr = new XMLHttpRequest();
    xhr.open(
      "POST",
      `/api/files/${encodeFilePathForApi(targetDirectory)}?type=upload&conflict=${strategy}`,
    );
    xhr.upload.onprogress = (event) => {
      if (event.lengthComputable && event.total > 0) {
        onProgress(Math.round((event.loaded / event.total) * 100));
      }
    };
    xhr.onerror = () => reject(new Error("Network error while uploading files"));
    xhr.onabort = () => reject(new Error("Upload cancelled"));
    xhr.onload = () => {
      let data: UploadResponse = {};
      try {
        data = JSON.parse(xhr.responseText) as UploadResponse;
      } catch {
        if (xhr.responseText) data.error = xhr.responseText;
      }
      resolve({ status: xhr.status, data });
    };
    xhr.send(formData);
  });
}

function DismissButton({ onClick, title }: { onClick: () => void; title: string }) {
  return (
    <button
      type="button"
      onClick={onClick}
      title={title}
      aria-label={title}
      style={{ width: 24, height: 24, padding: 0, display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0, border: "none", borderRadius: 4, background: "none", color: "var(--text-dim)", cursor: "pointer" }}
      onMouseEnter={(event) => { event.currentTarget.style.color = "var(--text-muted)"; event.currentTarget.style.background = "var(--bg-hover)"; }}
      onMouseLeave={(event) => { event.currentTarget.style.color = "var(--text-dim)"; event.currentTarget.style.background = "none"; }}
    >
      <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" aria-hidden="true">
        <path d="m6 6 12 12" />
        <path d="m18 6-12 12" />
      </svg>
    </button>
  );
}

/**
 * 目录行右侧操作区里的 Git 按钮（排在「提及」之后、在访达之前）。
 *
 * 一个项目目录里可以并排着多个各自带 `.git` 的子项目，所以每个仓库行多一个按钮：
 * 点它把 Git 面板的仓库根切到这一行的目录，不会展开/收起这一行，也不会触发行内重命名。
 */
function GitRepositoryButton({ repositoryPath, name, onOpen, t }: {
  repositoryPath: string;
  name: string;
  onOpen: (repositoryPath: string) => void;
  t: Translate;
}) {
  return (
    <button
      type="button"
      className="file-row-git-button"
      title={`${t("files.openGitPanel")} · ${repositoryPath}`}
      aria-label={`${t("files.openGitPanel")}: ${name}`}
      onClick={(event) => {
        event.stopPropagation();
        onOpen(repositoryPath);
      }}
      onDoubleClick={(event) => event.stopPropagation()}
      style={{
        display: "flex",
        alignItems: "center",
        justifyContent: "center",
        padding: "0 5px",
        height: 20,
        background: "var(--bg-panel)",
        border: "1px solid var(--border)",
        color: "var(--text-muted)",
        cursor: "pointer",
      }}
    >
      <GitBranch size={14} strokeWidth={2} aria-hidden="true" />
    </button>
  );
}

function TreeNode({
  node,
  depth,
  cwd,
  onOpenFile,
  onFileRenamed,
  canRenamePath,
  onAtMention,
  onOpenRepositoryGitPanel,
  expandedPaths,
  onToggleExpanded,
  refreshToken,
  highlightedPaths,
  gitStatusByPath,
  changedDirectoryPaths,
  hideDotDirs,
  t,
}: {
  node: FileNode;
  depth: number;
  cwd: string;
  onOpenFile: (filePath: string, fileName: string, options?: OpenFileOptions) => void;
  onFileRenamed?: (oldPath: string, newPath: string, newName: string) => void;
  canRenamePath?: (filePath: string) => boolean;
  onAtMention?: (relativePath: string, isDir: boolean) => void;
  onOpenRepositoryGitPanel?: (repositoryPath: string) => void;
  expandedPaths: Set<string>;
  onToggleExpanded: (fullPath: string, open: boolean) => void;
  refreshToken: string;
  highlightedPaths: Set<string>;
  gitStatusByPath: Map<string, GitFileStatus>;
  changedDirectoryPaths: Set<string>;
  hideDotDirs: boolean;
  t: Translate;
}) {
  const open = expandedPaths.has(node.fullPath);
  const highlighted = highlightedPaths.has(node.fullPath);
  const normalizedPath = normalizeFilePathSlashes(node.fullPath);
  const gitStatus = gitStatusByPath.get(normalizedPath);
  const containsGitChanges = node.isDir && (
    gitStatus !== undefined || changedDirectoryPaths.has(normalizedPath)
  );
  const [children, setChildren] = useState<FileNode[]>(node.children ?? []);
  const [loaded, setLoaded] = useState(node.loaded ?? false);
  const [loading, setLoading] = useState(false);
  const [hovered, setHovered] = useState(false);
  const [renaming, setRenaming] = useState(false);
  const [renameValue, setRenameValue] = useState(node.name);
  const [renamePending, setRenamePending] = useState(false);
  const [renameError, setRenameError] = useState<string | null>(null);
  const renameInputRef = useRef<HTMLInputElement>(null);
  const renamePendingRef = useRef(false);
  const renameCancelledRef = useRef(false);

  const loadChildren = useCallback(async (force = false) => {
    if (loaded && !force) return;
    setLoading(true);
    try {
      const entries = await fetchEntries(node.fullPath, cwd, hideDotDirs);
      setChildren(entries);
      setLoaded(true);
    } catch {
      // ignore
    } finally {
      setLoading(false);
    }
  }, [loaded, node.fullPath, cwd, hideDotDirs]);

  // Re-fetch children when the tree refreshes and the directory is open.
  useEffect(() => {
    if (open && loaded) {
      loadChildren(true);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [refreshToken]);

  const handleClick = useCallback(() => {
    if (renaming) return;
    if (node.isDir) {
      const next = !open;
      onToggleExpanded(node.fullPath, next);
      if (next && !loaded) loadChildren();
    } else {
      onOpenFile(node.fullPath, node.name);
    }
  }, [renaming, node.isDir, node.fullPath, node.name, loaded, open, loadChildren, onOpenFile, onToggleExpanded]);

  useEffect(() => {
    if (!renaming) return;
    const frame = requestAnimationFrame(() => {
      const input = renameInputRef.current;
      if (!input) return;
      input.focus();
      const extensionIndex = node.name.lastIndexOf(".");
      input.setSelectionRange(0, extensionIndex > 0 ? extensionIndex : node.name.length);
    });
    return () => cancelAnimationFrame(frame);
  }, [node.name, renaming]);

  const renameErrorMessage = useCallback((code?: string, fallback?: string) => {
    if (code === "invalid_name") return t("files.renameInvalid");
    if (code === "already_exists") return t("files.renameExists");
    if (code === "not_found") return t("files.renameMissing");
    return fallback || t("files.renameFailed");
  }, [t]);

  const startRename = useCallback(() => {
    // 折叠后的包行代表整条包链，重命名只能对最内层目录生效，改哪个都不对。
    if (node.packageChain) return;
    if (canRenamePath && !canRenamePath(node.fullPath)) {
      setRenameError(t("files.renameUnsaved"));
      return;
    }
    renameCancelledRef.current = false;
    setRenameValue(node.name);
    setRenameError(null);
    setRenaming(true);
  }, [canRenamePath, node.fullPath, node.name, t]);

  const commitRename = useCallback(async () => {
    if (renamePendingRef.current || renameCancelledRef.current) return;
    const name = renameValue.trim();
    if (name === node.name) {
      setRenaming(false);
      setRenameError(null);
      return;
    }
    if (!name || name === "." || name === ".." || /[\\/\0]/.test(name)) {
      setRenameError(t("files.renameInvalid"));
      requestAnimationFrame(() => renameInputRef.current?.focus());
      return;
    }
    if (canRenamePath && !canRenamePath(node.fullPath)) {
      setRenameError(t("files.renameUnsaved"));
      requestAnimationFrame(() => renameInputRef.current?.focus());
      return;
    }

    renamePendingRef.current = true;
    setRenamePending(true);
    setRenameError(null);
    try {
      const res = await fetch(`/api/files/${encodeFilePathForApi(node.fullPath)}`, {
        method: "PATCH",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ name }),
      });
      const data = await res.json().catch(() => ({})) as RenameResponse;
      if (!res.ok || !data.path) {
        throw Object.assign(new Error(renameErrorMessage(data.code, data.error)), { renameCode: data.code });
      }
      onFileRenamed?.(node.fullPath, data.path, data.name ?? name);
      setRenaming(false);
      setRenameError(null);
    } catch (reason) {
      const error = reason as Error & { renameCode?: string };
      setRenameError(renameErrorMessage(error.renameCode, error.message));
      requestAnimationFrame(() => renameInputRef.current?.focus());
    } finally {
      renamePendingRef.current = false;
      setRenamePending(false);
    }
  }, [canRenamePath, node.fullPath, node.name, onFileRenamed, renameErrorMessage, renameValue, t]);

  const cancelRename = useCallback(() => {
    renameCancelledRef.current = true;
    setRenaming(false);
    setRenameError(null);
  }, []);

  return (
    <div>
      <div
        onClick={handleClick}
        onMouseEnter={() => setHovered(true)}
        onMouseLeave={() => setHovered(false)}
        style={{
          position: "relative",
          display: "flex",
          alignItems: "center",
          gap: 4,
          paddingLeft: 8 + depth * 14,
          paddingRight: 8,
          height: 24,
          cursor: "pointer",
          background: hovered ? "var(--bg-hover)" : "transparent",
          borderRadius: 4,
          userSelect: "none",
        }}
      >
        {node.isDir && (
          <span
            style={{
              width: 10,
              height: 10,
              flexShrink: 0,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
              color: "var(--text-dim)",
            }}
          >
            {loading ? (
              <TreeLoadingSpinner size={10} label={t("files.loading")} />
            ) : (
              <svg
                width="10" height="10" viewBox="0 0 10 10" fill="none"
                stroke="var(--text-dim)" strokeWidth="1.8" strokeLinecap="round" strokeLinejoin="round"
                style={{ flexShrink: 0, transform: open ? "rotate(90deg)" : "none", transition: "transform 0.1s" }}
              >
                <polyline points="3 2 7 5 3 8" />
              </svg>
            )}
          </span>
        )}
        {!node.isDir && <span style={{ width: 10, flexShrink: 0 }} />}
        <span style={{ flexShrink: 0, display: "flex", alignItems: "center" }}>
          {node.isDir ? <FolderIcon size={14} open={open} /> : getFileIcon(node.name, 14)}
        </span>
        {renaming ? (
          <input
            ref={renameInputRef}
            value={renameValue}
            disabled={renamePending}
            aria-label={t("i18n.rename")}
            onChange={(event) => setRenameValue(event.target.value)}
            onClick={(event) => event.stopPropagation()}
            onDoubleClick={(event) => event.stopPropagation()}
            onBlur={() => { void commitRename(); }}
            onKeyDown={(event) => {
              if (event.key === "Enter") {
                event.preventDefault();
                event.currentTarget.blur();
              } else if (event.key === "Escape") {
                event.preventDefault();
                event.stopPropagation();
                cancelRename();
              }
            }}
            style={{
              minWidth: 0,
              height: 20,
              flex: 1,
              padding: "0 4px",
              border: `1px solid ${renameError ? "#f87171" : "var(--accent)"}`,
              borderRadius: 3,
              outline: "none",
              background: "var(--bg)",
              color: "var(--text)",
              fontSize: 12,
            }}
          />
        ) : (
          <span
            onClick={(event) => {
              if (event.detail > 1) event.stopPropagation();
            }}
            onDoubleClick={(event) => {
              event.preventDefault();
              event.stopPropagation();
              startRename();
            }}
            style={{
              fontSize: 12,
              color: "var(--text)",
              overflow: "hidden",
              textOverflow: "ellipsis",
              whiteSpace: "nowrap",
              flex: 1,
            }}
            title={node.fullPath}
          >
            {node.name}
          </span>
        )}
        {highlighted && (
          <span
            title={t("files.newlyUploaded")}
            aria-label={t("files.newlyUploaded")}
            style={{ width: 14, height: 14, flexShrink: 0, display: "flex", alignItems: "center", justifyContent: "center" }}
          >
            <span style={{ width: 6, height: 6, borderRadius: "50%", background: "#3b82f6" }} />
          </span>
        )}
        {!hovered && !node.isDir && gitStatus && (
          <GitStatusBadge status={gitStatus} t={t} />
        )}
        {!hovered && containsGitChanges && (
          <span
            title={t("files.containsChangedFiles")}
            aria-label={t("files.containsChangedFiles")}
            style={{
              width: 14,
              height: 14,
              flexShrink: 0,
              display: "flex",
              alignItems: "center",
              justifyContent: "center",
            }}
          >
            <span style={{ width: 6, height: 6, borderRadius: "50%", background: "#d6a84b" }} />
          </span>
        )}
        {hovered && !renaming && (
          <div
            className="file-row-actions"
            style={{
              position: "absolute",
              right: 4,
              top: "50%",
              transform: "translateY(-50%)",
              display: "flex",
              alignItems: "center",
            }}
          >
            {onAtMention && (
              <button
                type="button"
                onClick={(e) => {
                  e.stopPropagation();
                  onAtMention(getRelativeFilePath(node.fullPath, cwd), node.isDir);
                }}
                title={t("files.insertPath")}
                aria-label={t("files.mention")}
                style={{
                  display: "flex",
                  alignItems: "center",
                  justifyContent: "center",
                  padding: "0 5px",
                  height: 20,
                  background: "var(--bg-panel)",
                  border: "1px solid var(--border)",
                  color: "var(--text-muted)",
                  cursor: "pointer",
                }}
              >
                {/* 符号本身就是「提及」，文字标签只留在 title / aria-label 里。 */}
                <AtSign size={14} strokeWidth={2} aria-hidden="true" />
              </button>
            )}
            {node.isDir && node.gitRepository && onOpenRepositoryGitPanel && (
              <GitRepositoryButton
                repositoryPath={node.fullPath}
                name={node.name}
                onOpen={onOpenRepositoryGitPanel}
                t={t}
              />
            )}
            <RevealFileButton
              filePath={node.fullPath}
              style={{
                display: "flex",
                alignItems: "center",
                justifyContent: "center",
                padding: "0 5px",
                height: 20,
                background: "var(--bg-panel)",
                border: "1px solid var(--border)",
                color: "var(--text-muted)",
                cursor: "pointer",
                fontSize: 11,
                fontWeight: 600,
                whiteSpace: "nowrap",
                textDecoration: "none",
              }}
            />
          </div>
        )}
      </div>
      {renameError && (
        <div
          role="alert"
          style={{
            minHeight: 18,
            paddingLeft: 32 + depth * 14,
            paddingRight: 8,
            display: "flex",
            alignItems: "center",
            color: "#f87171",
            fontSize: 10,
            lineHeight: 1.3,
          }}
        >
          {renameError}
        </div>
      )}
      {node.isDir && open && (
        <div>
          {children.map((child) => (
              <TreeNode
                key={child.fullPath}
                node={child}
                depth={depth + 1}
                cwd={cwd}
                onOpenFile={onOpenFile}
                onFileRenamed={onFileRenamed}
                canRenamePath={canRenamePath}
                onAtMention={onAtMention}
                onOpenRepositoryGitPanel={onOpenRepositoryGitPanel}
                expandedPaths={expandedPaths}
                onToggleExpanded={onToggleExpanded}
                refreshToken={refreshToken}
                highlightedPaths={highlightedPaths}
                gitStatusByPath={gitStatusByPath}
                changedDirectoryPaths={changedDirectoryPaths}
                hideDotDirs={hideDotDirs}
                t={t}
              />
          ))}
          {children.length === 0 && loaded && (
            <div style={{ paddingLeft: 8 + (depth + 1) * 14, fontSize: 11, color: "var(--text-dim)", height: 22, display: "flex", alignItems: "center" }}>
              empty
            </div>
          )}
        </div>
      )}
    </div>
  );
}

type OpenFileOptions = { sourceSessionId?: string | null; modeHint?: "diff" };

type OpenFileHandler = (filePath: string, fileName: string, options?: OpenFileOptions) => void;

function ChangeRow({
  status,
  cwd,
  onOpenFile,
  t,
}: {
  status: GitFileStatus;
  cwd: string;
  onOpenFile: OpenFileHandler;
  t: Translate;
}) {
  const [hovered, setHovered] = useState(false);
  const name = getFileName(status.filePath);
  const rel = getRelativeFilePath(status.filePath, cwd);
  return (
    <div
      onClick={() => onOpenFile(status.filePath, name, { modeHint: "diff" })}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      title={status.filePath}
      style={{
        display: "flex",
        alignItems: "center",
        gap: 6,
        paddingLeft: 10,
        paddingRight: 8,
        height: 24,
        cursor: "pointer",
        background: hovered ? "var(--bg-hover)" : "transparent",
        borderRadius: 4,
        userSelect: "none",
      }}
    >
      <GitStatusBadge status={status} t={t} />
      <span style={{ flexShrink: 0, display: "flex", alignItems: "center", opacity: 0.85 }}>
        {getFileIcon(name, 13)}
      </span>
      <span
        style={{
          fontSize: 12,
          color: "var(--text)",
          overflow: "hidden",
          textOverflow: "ellipsis",
          whiteSpace: "nowrap",
          flex: 1,
        }}
      >
        {rel}
      </span>
    </div>
  );
}

export const FileExplorer = forwardRef<FileExplorerHandle, Props>(function FileExplorer({
  cwd,
  onOpenFile,
  onFileRenamed,
  canRenamePath,
  refreshKey,
  onAtMention,
  onAtMentions,
  onOpenRepositoryGitPanel,
  onUploadBusyChange,
  onLoadingChange,
  changesCollapsed,
  onChangesCountChange,
  hideDotDirs: propHideDotDirs,
  onHideDotDirsChange,
}, ref) {
  const { t } = useI18n();
  const [roots, setRoots] = useState<FileNode[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [expandedPaths, setExpandedPaths] = useState<Set<string>>(new Set());
  const migrateExpandedPaths = useCallback((oldPath: string, newPath: string) => {
    setExpandedPaths((previous) => {
      const next = new Set<string>();
      for (const expandedPath of previous) {
        if (expandedPath === oldPath || expandedPath.startsWith(`${oldPath}/`)) {
          next.add(`${newPath}${expandedPath.slice(oldPath.length)}`);
        } else {
          next.add(expandedPath);
        }
      }
      return next;
    });
  }, []);
  const [treeRefreshKey, setTreeRefreshKey] = useState(0);
  const [highlightedPaths, setHighlightedPaths] = useState<Set<string>>(new Set());
  const [gitFiles, setGitFiles] = useState<GitFileStatus[]>([]);
  const [gitLineStats, setGitLineStats] = useState({ additions: 0, deletions: 0 });
  const [uploadPhase, setUploadPhase] = useState<UploadPhase>("idle");
  const [uploadProgress, setUploadProgress] = useState(0);
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [uploadSummary, setUploadSummary] = useState<UploadSummary | null>(null);
  const [pendingConflict, setPendingConflict] = useState<PendingConflict | null>(null);
  const [hideDotDirs, setHideDotDirs] = useState(propHideDotDirs ?? false);

  // Sync local state with prop changes
  useEffect(() => {
    if (typeof propHideDotDirs === "boolean" && propHideDotDirs !== hideDotDirs) {
      setHideDotDirs(propHideDotDirs);
    }
  }, [propHideDotDirs]);
  const prevCwdRef = useRef<string | null>(null);
  const uploadInputRef = useRef<HTMLInputElement>(null);
  const refreshToken = `${refreshKey ?? 0}:${treeRefreshKey}`;
  const uploadBusy = uploadPhase !== "idle";

  const gitStatusByPath = useMemo(() => new Map(
    gitFiles.map((status) => [normalizeFilePathSlashes(status.filePath), status]),
  ), [gitFiles]);

  const changedDirectoryPaths = useMemo(() => {
    const directories = new Set<string>();
    const normalizedCwd = normalizeFilePathSlashes(cwd).replace(/\/$/, "");
    for (const status of gitFiles) {
      let directory = getFileDirectory(normalizeFilePathSlashes(status.filePath));
      while (directory === normalizedCwd || directory.startsWith(`${normalizedCwd}/`)) {
        directories.add(directory);
        if (directory === normalizedCwd) break;
        const parent = getFileDirectory(directory);
        if (parent === directory) break;
        directory = parent;
      }
    }
    return directories;
  }, [cwd, gitFiles]);

  const handleToggleExpanded = useCallback((fullPath: string, open: boolean) => {
    setExpandedPaths((prev) => {
      const next = new Set(prev);
      if (open) next.add(fullPath); else next.delete(fullPath);
      return next;
    });
  }, []);

  const handleToggleHideDotDirs = useCallback(() => {
    setHideDotDirs((prev) => !prev);
  }, []);

  const applyUploadResult = useCallback((data: UploadResponse) => {
    const uploaded = data.uploaded ?? [];
    const skipped = data.skipped ?? [];
    const errors = data.errors ?? [];
    setUploadSummary({ uploaded, skipped, errors });

    if (uploaded.length > 0) {
      setHighlightedPaths(new Set(uploaded.map((name) => joinFilePath(cwd, name))));
      setTreeRefreshKey((key) => key + 1);
    }
  }, [cwd]);

  const performUpload = useCallback(async (
    files: File[],
    strategy: UploadConflictStrategy,
  ) => {
    setPendingConflict(null);
    setUploadError(null);
    setUploadProgress(0);
    setUploadPhase("uploading");

    try {
      const { status, data } = await uploadFiles(cwd, files, strategy, setUploadProgress);
      if (status === 409 && data.conflicts?.length) {
        setPendingConflict({
          files,
          conflicts: data.conflicts,
          nonReplaceable: data.nonReplaceable ?? [],
        });
        return;
      }
      if (status < 200 || status >= 300) {
        throw new Error(data.error ?? `Upload failed (HTTP ${status})`);
      }
      setUploadProgress(100);
      applyUploadResult(data);
    } catch (uploadFailure) {
      setUploadError(uploadFailure instanceof Error ? uploadFailure.message : String(uploadFailure));
    } finally {
      setUploadPhase("idle");
    }
  }, [applyUploadResult, cwd]);

  const prepareUpload = useCallback(async (files: File[]) => {
    if (files.length === 0 || uploadBusy) return;
    setUploadSummary(null);
    setHighlightedPaths(new Set());
    setPendingConflict(null);
    setUploadError(null);
    setUploadProgress(0);
    setUploadPhase("checking");

    try {
      const res = await fetch(
        `/api/files/${encodeFilePathForApi(cwd)}?type=upload-check`,
        {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ fileNames: files.map((file) => file.name) }),
        },
      );
      const data = await res.json().catch(() => ({})) as UploadResponse;
      if (!res.ok) throw new Error(data.error ?? `Upload check failed (HTTP ${res.status})`);

      if (data.conflicts?.length) {
        setPendingConflict({
          files,
          conflicts: data.conflicts,
          nonReplaceable: data.nonReplaceable ?? [],
        });
        return;
      }

      await performUpload(files, "error");
    } catch (uploadFailure) {
      setUploadError(uploadFailure instanceof Error ? uploadFailure.message : String(uploadFailure));
    } finally {
      setUploadPhase("idle");
    }
  }, [cwd, performUpload, uploadBusy]);

  const handleUploadInput = useCallback((event: React.ChangeEvent<HTMLInputElement>) => {
    const files = Array.from(event.target.files ?? []);
    event.target.value = "";
    void prepareUpload(files);
  }, [prepareUpload]);

  useImperativeHandle(ref, () => ({
    openUploadPicker() {
      if (!uploadBusy) uploadInputRef.current?.click();
    },
    setHideDotDirs(updater) {
      // updater is either a boolean or a function
      const next = typeof updater === "function" ? updater(hideDotDirs) : updater;
      setHideDotDirs(next);
      onHideDotDirsChange?.(next);
    },
    getHideDotDirs() {
      return hideDotDirs;
    },
  }), [uploadBusy, hideDotDirs, onHideDotDirsChange]);

  useEffect(() => {
    onUploadBusyChange?.(uploadBusy);
  }, [onUploadBusyChange, uploadBusy]);

  useEffect(() => () => onUploadBusyChange?.(false), [onUploadBusyChange]);

  useEffect(() => {
    onLoadingChange?.(loading);
  }, [loading, onLoadingChange]);

  useEffect(() => () => onLoadingChange?.(false), [onLoadingChange]);

  useEffect(() => {
    const cwdChanged = prevCwdRef.current !== cwd;
    prevCwdRef.current = cwd;

    // Reset expanded state only when cwd changes, not on refreshKey bumps
    if (cwdChanged) {
      setExpandedPaths(new Set());
      setHighlightedPaths(new Set());
      setUploadSummary(null);
      setPendingConflict(null);
      setUploadError(null);
    }

    // 换项目时旧根节点必须立刻清掉，否则新目录还在请求中会先闪一帧旧项目的文件；
    // 只是刷新时保留现有列表，转圈交给标题行的箭头，列表不跳。
    if (cwdChanged) setRoots([]);
    setLoading(true);
    setError(null);
    let cancelled = false;
    fetchEntries(cwd, cwd, hideDotDirs)
      .then((entries) => { if (!cancelled) setRoots(entries); })
      .catch((e) => { if (!cancelled) setError(e instanceof Error ? e.message : String(e)); })
      .finally(() => { if (!cancelled) setLoading(false); });
    return () => { cancelled = true; };
  }, [cwd, refreshKey, treeRefreshKey, hideDotDirs]);

  useEffect(() => {
    let cancelled = false;
    fetchGitStatus(cwd)
      .then((status) => {
        if (!cancelled) {
          setGitFiles(status.isGitRepository ? status.files : []);
          setGitLineStats(status.isGitRepository
            ? { additions: status.additions, deletions: status.deletions }
            : { additions: 0, deletions: 0 });
        }
      })
      .catch(() => {
        if (!cancelled) {
          setGitFiles([]);
          setGitLineStats({ additions: 0, deletions: 0 });
        }
      });
    return () => { cancelled = true; };
  }, [cwd, refreshKey, treeRefreshKey]);

  useEffect(() => {
    onChangesCountChange?.(gitFiles.length);
  }, [gitFiles, onChangesCountChange]);

  const showUploadFeedback = uploadBusy || pendingConflict !== null || uploadError !== null || uploadSummary !== null;

  const addUploadedFilesToChat = useCallback(() => {
    if (!uploadSummary || uploadSummary.uploaded.length === 0) return;
    onAtMentions?.(
      uploadSummary.uploaded.map((name) => getRelativeFilePath(joinFilePath(cwd, name), cwd)),
    );
  }, [cwd, onAtMentions, uploadSummary]);

  return (
    <div style={{ minHeight: "100%" }}>
      <input ref={uploadInputRef} type="file" multiple hidden onChange={handleUploadInput} />
      {showUploadFeedback && (
        <div style={{ padding: "6px 8px", borderBottom: "1px solid var(--border)" }}>
        {uploadBusy && (
          <div role="status" aria-live="polite" aria-label={uploadPhase === "checking" ? t("files.checking") : t("files.uploading", { progress: uploadProgress })}>
            <div style={{ display: "flex", alignItems: "center", justifyContent: "space-between", gap: 8, minHeight: 14, color: "var(--text-muted)" }}>
              {uploadPhase === "checking" ? (
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" style={{ animation: "spin 0.8s linear infinite" }} aria-hidden="true">
                  <path d="M21 12a9 9 0 1 1-5.7-8.4" />
                </svg>
              ) : (
                <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                  <path d="M12 16V4" />
                  <path d="m7 9 5-5 5 5" />
                  <path d="M5 20h14" />
                </svg>
              )}
              {uploadPhase === "uploading" && <span style={{ fontSize: 10 }}>{uploadProgress}%</span>}
            </div>
            {uploadPhase === "uploading" && (
              <div style={{ height: 3, marginTop: 4, overflow: "hidden", borderRadius: 2, background: "var(--border)" }}>
                <div style={{ width: `${uploadProgress}%`, height: "100%", background: "var(--text-muted)", transition: "width 120ms ease" }} />
              </div>
            )}
          </div>
        )}

        {pendingConflict && (
          <div role="alert" style={{ padding: 7, border: "1px solid color-mix(in srgb, #f59e0b 55%, var(--border))", borderRadius: 4, background: "color-mix(in srgb, #f59e0b 9%, var(--bg-panel))" }}>
            <div style={{ fontSize: 11, color: "var(--text)", lineHeight: 1.35, overflowWrap: "anywhere" }}>
              {t("files.conflictSummary", { count: pendingConflict.conflicts.length, countSuffix: pendingConflict.conflicts.length === 1 ? "" : "s", files: pendingConflict.conflicts.join(", ") })}
            </div>
            {pendingConflict.nonReplaceable.length > 0 && (
              <div style={{ marginTop: 3, fontSize: 10, color: "#f59e0b", lineHeight: 1.35, overflowWrap: "anywhere" }}>
                {t("files.cannotReplace", { files: pendingConflict.nonReplaceable.join(", ") })}
              </div>
            )}
            <div style={{ display: "flex", gap: 5, marginTop: 7 }}>
              <button type="button" onClick={() => void performUpload(pendingConflict.files, "overwrite")} style={{ height: 22, padding: "0 7px", border: "1px solid #ef4444", borderRadius: 4, background: "transparent", color: "#ef4444", cursor: "pointer", fontSize: 10 }}>
                {t("files.replace")}
              </button>
              <button type="button" onClick={() => void performUpload(pendingConflict.files, "skip")} style={{ height: 22, padding: "0 7px", border: "1px solid var(--border)", borderRadius: 4, background: "var(--bg-panel)", color: "var(--text)", cursor: "pointer", fontSize: 10 }}>
                {t("files.skipExisting")}
              </button>
              <button type="button" onClick={() => setPendingConflict(null)} style={{ height: 22, padding: "0 7px", border: "none", borderRadius: 4, background: "transparent", color: "var(--text-muted)", cursor: "pointer", fontSize: 10 }}>
                {t("files.cancel")}
              </button>
            </div>
          </div>
        )}

        {uploadError && (
          <div role="alert" style={{ display: "flex", alignItems: "flex-start", gap: 6, fontSize: 11, lineHeight: 1.35, color: "#f87171" }}>
            <span style={{ minWidth: 0, flex: 1, overflowWrap: "anywhere" }}>{uploadError}</span>
            <DismissButton onClick={() => setUploadError(null)} title={t("files.dismissError")} />
          </div>
        )}

        {uploadSummary && (
          <div aria-live="polite">
            <div style={{ display: "flex", alignItems: "center", gap: 8, minHeight: 22, fontSize: 11 }}>
              <div style={{ minWidth: 0, flex: 1, display: "flex", alignItems: "center", gap: 8 }}>
                {uploadSummary.uploaded.length > 0 && (
                  <span title={`${uploadSummary.uploaded.length} uploaded`} aria-label={`${uploadSummary.uploaded.length} uploaded`} style={{ display: "flex", alignItems: "center", gap: 3, color: "#22c55e" }}>
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                      <path d="m5 12 4 4L19 6" />
                    </svg>
                    <span>{uploadSummary.uploaded.length}</span>
                  </span>
                )}
                {uploadSummary.skipped.length > 0 && (
                  <span title={`${uploadSummary.skipped.length} skipped`} aria-label={`${uploadSummary.skipped.length} skipped`} style={{ display: "flex", alignItems: "center", gap: 3, color: "var(--text-dim)" }}>
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
                      <circle cx="12" cy="12" r="9" />
                      <path d="M8 12h8" />
                    </svg>
                    <span>{uploadSummary.skipped.length}</span>
                  </span>
                )}
                {uploadSummary.errors.length > 0 && (
                  <span title={`${uploadSummary.errors.length} failed`} aria-label={`${uploadSummary.errors.length} failed`} style={{ display: "flex", alignItems: "center", gap: 3, color: "#f87171" }}>
                    <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
                      <path d="M12 3 2.5 20h19L12 3Z" />
                      <path d="M12 9v4" />
                      <path d="M12 17h.01" />
                    </svg>
                    <span>{uploadSummary.errors.length}</span>
                  </span>
                )}
              </div>
              {uploadSummary.uploaded.length > 0 && onAtMentions && (
                <button
                  type="button"
                  onClick={addUploadedFilesToChat}
                  title={uploadSummary.uploaded.length === 1 ? t("files.addUploadedFile") : t("files.addAllUploadedFiles")}
                  aria-label={uploadSummary.uploaded.length === 1 ? t("files.addUploadedFile") : t("files.addAllUploadedFiles")}
                  style={{ height: 22, padding: "0 7px", display: "flex", alignItems: "center", justifyContent: "center", flexShrink: 0, border: "1px solid var(--border)", borderRadius: 4, background: "var(--bg-panel)", color: "var(--accent)", cursor: "pointer" }}
                >
                  {/* 与文件树行的提及按钮一致：符号代替「提及」文字。 */}
                  <AtSign size={14} strokeWidth={2} aria-hidden="true" />
                </button>
              )}
              <DismissButton onClick={() => setUploadSummary(null)} title={t("files.dismissUploadResults")} />
            </div>
            {uploadSummary.errors.map((item) => (
              <div key={item.name} title={item.error} style={{ display: "flex", alignItems: "center", gap: 4, marginTop: 3, minWidth: 0, fontSize: 10, color: "#f87171" }}>
                <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" style={{ flexShrink: 0 }} aria-hidden="true">
                  <circle cx="12" cy="12" r="9" />
                  <path d="M12 8v5" />
                  <path d="M12 17h.01" />
                </svg>
                <span style={{ minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap" }}>{item.name}</span>
              </div>
            ))}
          </div>
        )}
        </div>
      )}

      {!changesCollapsed && gitFiles.length > 0 && (
        <div style={{ padding: "0 4px 2px" }}>
          <div
            aria-label={t("files.changeStats", {
              count: gitFiles.length,
              additions: gitLineStats.additions,
              deletions: gitLineStats.deletions,
            })}
            style={{ display: "flex", alignItems: "center", gap: 6, height: 24, padding: "0 10px", fontSize: 12 }}
          >
            <span style={{ color: "var(--text-dim)" }}>
              {t("files.changedCount", { count: gitFiles.length })}
            </span>
            <span style={{ color: GIT_STATUS_COLORS.added, fontFamily: "var(--font-mono)" }}>+{gitLineStats.additions}</span>
            <span style={{ color: GIT_STATUS_COLORS.deleted, fontFamily: "var(--font-mono)" }}>-{gitLineStats.deletions}</span>
          </div>
          {gitFiles.map((status) => (
            <ChangeRow key={status.filePath} status={status} cwd={cwd} onOpenFile={onOpenFile} t={t} />
          ))}
        </div>
      )}

      {(changesCollapsed || gitFiles.length === 0) && (
        <div>
          <div style={{ padding: "2px 4px" }}>
          {error ? (
            <div style={{ padding: "8px 12px", fontSize: 11, color: "#f87171" }}>{error}</div>
          ) : loading && roots.length === 0 ? (
            <div style={{ padding: "8px 12px", fontSize: 11, color: "var(--text-dim)" }}>{t("files.loading")}</div>
          ) : (
            roots.map((node) => (
              <TreeNode
                key={node.fullPath}
                node={node}
                depth={0}
                cwd={cwd}
                onOpenFile={onOpenFile}
                onFileRenamed={(oldPath, newPath, newName) => {
                  setTreeRefreshKey((key) => key + 1);
                  migrateExpandedPaths(oldPath, newPath);
                  onFileRenamed?.(oldPath, newPath, newName);
                }}
                canRenamePath={canRenamePath}
                onAtMention={onAtMention}
                onOpenRepositoryGitPanel={onOpenRepositoryGitPanel}
                expandedPaths={expandedPaths}
                onToggleExpanded={handleToggleExpanded}
                refreshToken={refreshToken}
                highlightedPaths={highlightedPaths}
                gitStatusByPath={gitStatusByPath}
                changedDirectoryPaths={changedDirectoryPaths}
                hideDotDirs={hideDotDirs}
                t={t}
              />
            ))
          )}
          {!loading && !error && roots.length === 0 && (
            <div style={{ padding: "8px 12px", fontSize: 11, color: "var(--text-dim)" }}>
              {t("files.noFiles")}
            </div>
          )}
          </div>
        </div>
      )}
    </div>
  );
});
