"use client";

import { memo, useMemo, useState } from "react";
import ReactMarkdown, { type Components, type Options as ReactMarkdownOptions } from "react-markdown";
import remarkGfm from "remark-gfm";
import { useI18n } from "@/hooks/useI18n";
import { copyText } from "@/lib/clipboard";
import { resolveLocalFileHref } from "@/lib/file-links";
import { encodeFilePathForApi } from "@/lib/file-paths";

/**
 * 手机端的 markdown 渲染。
 *
 * 边界刻意画在这里：只带 react-markdown + remark-gfm。
 * 语法高亮（react-syntax-highlighter）、KaTeX、mermaid、rehype-raw 全部不进 /m，
 * 也不复用桌面渲染器 MarkdownBody（它把上面几样都拉进包体）。
 * 样式复用 globals.css 里已有的 .markdown-body 系列，手机和桌面看起来同源。
 *
 * 没有 rehype-raw 意味着模型输出的原始 HTML 不会被当成标签执行，只会作为文本落地。
 */

// 单波浪线删除线会吃掉中文区间「5~7U」「100~200倍」（#385），与桌面保持同一份选项。
// 模块级常量：配置对象身份稳定，ReactMarkdown 不会因此重新解析。
const REMARK_PLUGINS: ReactMarkdownOptions["remarkPlugins"] = [[remarkGfm, { singleTilde: false }]];

/** 代码块：手机上不做语法高亮，只留语言标签和复制按钮（手机上手动选中代码很难）。 */
const MobileCodeBlock = memo(function MobileCodeBlock({ code, lang }: { code: string; lang: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);

  return (
    <div className="markdown-code-block">
      <div className="markdown-code-header">
        <span className="markdown-code-lang">{lang || "text"}</span>
        <div className="markdown-code-actions">
          <button
            type="button"
            className="markdown-code-action mobile-tap"
            onClick={() => {
              void copyText(code).then(() => {
                setCopied(true);
                window.setTimeout(() => setCopied(false), 1500);
              }).catch(() => {
                // 复制失败（无剪贴板权限）静默处理：代码本身仍然可读可选中。
              });
            }}
          >
            {copied ? t("i18n.copied") : t("i18n.copy")}
          </button>
        </div>
      </div>
      <pre
        style={{
          margin: 0, padding: "11px 13px", fontSize: 12, lineHeight: 1.55,
          overflowX: "auto", background: "color-mix(in srgb, var(--bg) 92%, var(--bg-panel))",
        }}
      >
        <code style={{ fontFamily: "var(--font-mono)" }}>{code}</code>
      </pre>
    </div>
  );
});

interface MobileMarkdownProps {
  text: string;
  /** 会话 cwd：把相对路径的图片解析成本机文件接口地址。 */
  cwd?: string | null;
  /** 流式追加中：在最后一块内容后面画输出光标（见 .mobile-streaming 样式）。 */
  streaming?: boolean;
}

export const MobileMarkdown = memo(function MobileMarkdown({ text, cwd, streaming }: MobileMarkdownProps) {
  const components = useMemo<Components>(() => ({
    code({ className, children, ...props }) {
      delete props.node;
      const lang = className?.replace("language-", "").toLowerCase() ?? "";
      const raw = String(children);
      const isBlock = className?.includes("language-") || raw.includes("\n");
      if (isBlock) return <MobileCodeBlock code={raw.replace(/\n$/, "")} lang={lang} />;
      return <code className="markdown-inline-code" {...props}>{children}</code>;
    },
    pre({ children }) {
      // 外壳由上面的 code 渲染器负责，这里不再套一层 <pre>。
      return <>{children}</>;
    },
    a({ href, children, ...props }) {
      delete props.node;
      // 手机打不开本机文件，把这类链接降级成等宽文本，而不是把遥控页导到一个 404。
      if (resolveLocalFileHref(href, cwd ?? undefined)) {
        return (
          <span style={{ fontFamily: "var(--font-mono)", color: "var(--text-muted)", overflowWrap: "anywhere" }}>
            {children}
          </span>
        );
      }
      return <a href={href} {...props} target="_blank" rel="noopener noreferrer">{children}</a>;
    },
    img({ src, alt, ...props }) {
      delete props.node;
      const filePath = typeof src === "string" ? resolveLocalFileHref(src, cwd ?? undefined) : null;
      // 相对路径的图片走文件接口；路径不在允许根目录内时只会显示 alt。
      const imageSrc = filePath ? `/api/files/${encodeFilePathForApi(filePath)}?type=read` : src;
      // eslint-disable-next-line @next/next/no-img-element
      return <img src={imageSrc} alt={alt ?? ""} loading="lazy" decoding="async" {...props} />;
    },
    table({ children }) {
      return (
        <div className="markdown-table-wrap">
          <table>{children}</table>
        </div>
      );
    },
  }), [cwd]);

  return (
    <div className={["markdown-body", streaming ? "mobile-streaming" : ""].filter(Boolean).join(" ")}>
      <ReactMarkdown remarkPlugins={REMARK_PLUGINS} components={components}>{text}</ReactMarkdown>
    </div>
  );
});
