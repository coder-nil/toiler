"use client";

import { useMemo, type MouseEvent } from "react";
import ReactMarkdown, { type Components } from "react-markdown";
import { resolveLocalFileTarget, type FileOpenLocation } from "@/lib/file-links";
import { encodeFilePathForApi } from "@/lib/file-paths";
import { isOpenableExternalUrl, takeOverExternalLink } from "@/lib/open-external-url";
import {
  markdownRehypePlugins,
  markdownRehypePluginsWithoutRaw,
  markdownRemarkPlugins,
  normalizeDisplayMath,
} from "@/lib/markdown";
import { MermaidBlock, CodeBlock } from "./MermaidBlock";

interface MarkdownBodyProps {
  children: string;
  className?: string;
  isStreaming?: boolean;
  cwd?: string;
  onOpenFile?: (filePath: string, location?: FileOpenLocation) => void;
  allowHtml?: boolean;
}

export function MarkdownBody({ children, className, isStreaming, cwd, onOpenFile, allowHtml = true }: MarkdownBodyProps) {
  const normalizedMarkdown = useMemo(() => {
    const normalized = normalizeDisplayMath(children);
    // remark treats raw HTML nodes as non-renderable without rehypeRaw. Encode
    // their opening delimiter so user-authored XML/HTML remains visible text.
    return allowHtml ? normalized : normalized.replaceAll("<", "&lt;");
  }, [allowHtml, children]);
  // Stable renderer identities keep stateful blocks mounted across message hover updates.
  const components = useMemo<Components>(() => ({
    code({ className, children, ...props }) {
      const lang = className?.replace("language-", "").toLowerCase() ?? "";
      const raw = String(children);
      const isBlock = className?.includes("language-") || raw.includes("\n");
      if (isBlock) {
        if (lang === "mermaid") {
          return <MermaidBlock code={raw.replace(/\n$/, "")} isStreaming={isStreaming} />;
        }
        return <CodeBlock code={raw.replace(/\n$/, "")} lang={lang} isStreaming={isStreaming} />;
      }
      return (
        <code
          className="markdown-inline-code"
          {...props}
        >
          {children}
        </code>
      );
    },
    pre({ children }) {
      return <>{children}</>;
    },
    a({ href, children, ...props }) {
      // `node` is react-markdown metadata, not a DOM attribute.
      delete props.node;
      const externalHref = typeof href === "string" && isOpenableExternalUrl(href) ? href : null;
      // 桌面端（Tauri WebView）不会为 target="_blank" 开窗，链接点了等于没反应；
      // 这里拦下来交给系统浏览器（浏览器里保留原生行为）。
      const handleExternalOpen = (event: MouseEvent<HTMLAnchorElement>) => {
        if (externalHref) takeOverExternalLink(event, externalHref);
      };
      const fileTarget = onOpenFile ? resolveLocalFileTarget(href, cwd) : null;
      const openFile = onOpenFile;
      if (!fileTarget || !openFile) {
        return (
          <a href={href} {...props} target="_blank" rel="noopener noreferrer" onClick={handleExternalOpen}>
            {children}
          </a>
        );
      }

      const handleClick = (event: MouseEvent<HTMLAnchorElement>) => {
        if (event.defaultPrevented || event.button !== 0) return;
        if (event.metaKey || event.ctrlKey || event.shiftKey || event.altKey) return;
        const target = event.currentTarget.getAttribute("target");
        if (target && target !== "_self") return;
        event.preventDefault();
        openFile(fileTarget.filePath, { line: fileTarget.line, column: fileTarget.column });
      };

      return (
        <a href={href} {...props} onClick={handleClick}>
          {children}
        </a>
      );
    },
    img({ src, alt, ...props }) {
      delete props.node;
      const filePath = typeof src === "string" ? resolveLocalFileTarget(src, cwd)?.filePath ?? null : null;
      const imageSrc = filePath
        ? `/api/files/${encodeFilePathForApi(filePath)}?type=read`
        : src;
      // Dynamic local paths are served directly by the file API.
      // eslint-disable-next-line @next/next/no-img-element
      return <img src={imageSrc} alt={alt ?? ""} loading="lazy" {...props} />;
    },
    table({ children }) {
      return (
        <div className="markdown-table-wrap">
          <table>{children}</table>
        </div>
      );
    },
  }), [cwd, isStreaming, onOpenFile]);

  return (
    <div className={["markdown-body", className].filter(Boolean).join(" ")}>
      <ReactMarkdown
        remarkPlugins={markdownRemarkPlugins}
        rehypePlugins={allowHtml ? markdownRehypePlugins : markdownRehypePluginsWithoutRaw}
        components={components}
      >
        {normalizedMarkdown}
      </ReactMarkdown>
    </div>
  );
}
