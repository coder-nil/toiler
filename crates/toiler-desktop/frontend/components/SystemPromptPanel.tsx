"use client";

import { Check, Copy, FileText } from "lucide-react";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { useI18n } from "@/hooks/useI18n";
import { copyText } from "@/lib/clipboard";
import {
  hasTaggedSections,
  splitPromptSections,
  type PromptSection,
} from "@/lib/prompt-sections";
import styles from "./SystemPromptPanel.module.css";

/**
 * 系统提示词面板（顶栏「系统」下拉的实体）。
 *
 * 两个视图，分工是「读」与「核」：
 *
 * - **分节**（默认）：按后端的分节结构拆成卡片，每类标签一种色，长清单渲染成
 *   条目行。提示词是「一段 preamble + 几个命名区块 + 一段语言指令」，区块名本身
 *   就是导航结构，平铺成一坨的话每次都得从头读。
 * - **原文**：逐字打印，供核对「面板显示的就是实际发出去的」。
 *
 * # 为什么不是 Markdown 渲染
 *
 * 区块边界就是 `<tools>` / `<rules>` / `<cwd>` 这些标签。交给 Markdown 渲染器，
 * 它们会被当成真 HTML：浏览器不认识 `<tools>`，标签在视图里直接消失，只剩内容
 * 失去归属；`<rules>` 里的 `edits[].oldText`、内联 AGENTS.md 里的 `#` 与 `**`
 * 也会被改写。所以这里自己按标签切分（`lib/prompt-sections.ts`），正文一个字节
 * 都不加工 —— 分节视图只改「怎么排」，原文视图保证「看到的就是原文」。
 *
 * 材质沿用顶栏下拉这一族的语言：`--bg-panel` 实底 + 一道 1px 边框，下沿就是抽屉口。
 */

interface SystemPromptPanelProps {
  /** `null` 表示还没取到；空串表示确实为空（工具全关时后端会清空）。 */
  prompt: string | null;
  loading: boolean;
}

/** 复制反馈的停留时间，与顶栏会话信息面板的复制按钮一致。 */
const COPIED_MS = 1400;

type PromptView = "sections" | "raw";

/**
 * 每类标签的色调与显示名。
 *
 * 颜色只用来区分「这是哪一类信息」，不承载褒贬：`rules` 的琥珀不是警告，
 * `project_context` 的绿也不是「通过」。所以全部取项目已有的语义 token，
 * 浅色/深色主题各自有对应的可读值。
 */
const SECTION_TONE: Record<string, { tone: string; labelKey: string }> = {
  tools: { tone: "accent", labelKey: "system.section.tools" },
  rules: { tone: "star", labelKey: "system.section.rules" },
  project_context: { tone: "live", labelKey: "system.section.project_context" },
  project_instructions: { tone: "live", labelKey: "system.section.project_instructions" },
  skills: { tone: "live", labelKey: "system.section.skills" },
  docs: { tone: "live", labelKey: "system.section.docs" },
  cwd: { tone: "dim", labelKey: "system.section.cwd" },
  env: { tone: "dim", labelKey: "system.section.env" },
};

function toneOf(section: PromptSection): string {
  if (section.tag === null) return "muted";
  return SECTION_TONE[section.tag]?.tone ?? "muted";
}

function labelKeyOf(section: PromptSection): string {
  if (section.tag === null) {
    switch (section.role) {
      case "epilogue": return "system.section.epilogue";
      case "note": return "system.section.note";
      default: return "system.section.preamble";
    }
  }
  return SECTION_TONE[section.tag]?.labelKey ?? "system.section.other";
}

/** 一行的列表条目：`- xxx`。缩进与行内空白原样保留在后面。 */
const LIST_ITEM = /^- (.*)$/;

interface BodyBlock {
  kind: "list" | "text";
  lines: string[];
}

/**
 * 把节正文切成「列表块 / 文本块」，只用于**排版**。
 *
 * 判据是行首 `- `：后端把工具清单、行为准则、AGENTS.md 路径都写成这一种形式，
 * 认它比猜 Markdown 语法稳。非 `- ` 开头的行按空行分段当普通文本，一字不改
 * （内联 AGENTS.md 里的 `#`、`**`、`<div>` 因此原样显示）。
 */
function toBlocks(body: string): BodyBlock[] {
  const blocks: BodyBlock[] = [];
  let current: BodyBlock | null = null;
  for (const line of body.split("\n")) {
    if (line.trim() === "") {
      current = null;
      continue;
    }
    const isList = LIST_ITEM.test(line);
    const kind: BodyBlock["kind"] = isList ? "list" : "text";
    if (current === null || current.kind !== kind) {
      current = { kind, lines: [] };
      blocks.push(current);
    }
    current.lines.push(isList ? LIST_ITEM.exec(line)![1] : line);
  }
  return blocks;
}

export function SystemPromptPanel({ prompt, loading }: SystemPromptPanelProps) {
  const { t, locale } = useI18n();
  const [copied, setCopied] = useState(false);
  const [view, setView] = useState<PromptView>("sections");
  const copiedTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);

  useEffect(() => () => {
    if (copiedTimerRef.current) clearTimeout(copiedTimerRef.current);
  }, []);

  // 换会话 / 重新加载后提示词会变，上一份的「已复制」不该留在新内容上。
  useEffect(() => {
    setCopied(false);
  }, [prompt]);

  const sections = useMemo(() => (prompt ? splitPromptSections(prompt) : []), [prompt]);
  // 切不出标签时（工具全关的纯文本提示词）分节视图会只剩一张卡片，没有意义。
  const structured = useMemo(() => hasTaggedSections(sections), [sections]);
  const activeView: PromptView = structured ? view : "raw";

  const stats = useMemo(() => {
    if (!prompt) return null;
    return {
      chars: prompt.length.toLocaleString(locale),
      lines: prompt.split("\n").length.toLocaleString(locale),
    };
  }, [prompt, locale]);

  const handleCopy = useCallback(() => {
    if (!prompt) return;
    // 复制永远是整段原文，与当前视图无关：要的是「能粘到别处」的那份字节。
    void copyText(prompt).then(() => {
      if (copiedTimerRef.current) clearTimeout(copiedTimerRef.current);
      setCopied(true);
      copiedTimerRef.current = setTimeout(() => setCopied(false), COPIED_MS);
    }).catch(() => {
      // 剪贴板不可用（无权限 / 非安全上下文）时保持原样，不弹错。
    });
  }, [prompt]);

  return (
    <section className={styles.panel} aria-label={t("system.prompt")}>
      <div className={styles.head}>
        <div className={styles.headInner}>
          <FileText size={13} strokeWidth={2} className={styles.headIcon} aria-hidden="true" />
          <span className={styles.title}>{t("system.prompt")}</span>
          {stats && (
            <span className={styles.meta}>
              <span>{t("system.chars", { count: stats.chars })}</span>
              <span className={styles.sep} aria-hidden="true">·</span>
              <span>{t("system.lines", { count: stats.lines })}</span>
            </span>
          )}
          {structured && (
            <div className={styles.segmented} role="group" aria-label={t("system.view")}>
              {(["sections", "raw"] as const).map((item) => (
                <button
                  key={item}
                  type="button"
                  className={styles.segment}
                  onClick={() => setView(item)}
                  aria-pressed={activeView === item}
                  data-active={activeView === item}
                >
                  {t(item === "sections" ? "system.view.sections" : "system.view.raw")}
                </button>
              ))}
            </div>
          )}
          <button
            type="button"
            className={styles.copy}
            onClick={handleCopy}
            disabled={!prompt}
            title={copied ? t("system.copied") : t("system.copy")}
            aria-label={copied ? t("system.copied") : t("system.copy")}
            data-copied={copied}
          >
            {copied
              ? <Check size={13} strokeWidth={2.4} aria-hidden="true" />
              : <Copy size={13} strokeWidth={2} aria-hidden="true" />}
            <span className={styles.copyLabel}>{copied ? t("system.copied") : t("system.copy")}</span>
          </button>
        </div>
      </div>

      {prompt ? (
        <div
          className={styles.body}
          // 长文本的滚动区可聚焦：键盘用户不必靠鼠标滚轮才能读完。
          tabIndex={0}
          role="region"
          aria-label={t("system.prompt")}
        >
          {activeView === "raw" ? (
            <div className={styles.text}>{prompt}</div>
          ) : (
            <div className={styles.sections}>
              {sections.map((section) => (
                <PromptSectionCard key={section.id} section={section} depth={0} />
              ))}
            </div>
          )}
        </div>
      ) : prompt === "" ? (
        <p className={styles.state}>{t("system.empty")}</p>
      ) : (
        <p className={styles.state} data-loading={loading}>
          {loading && <span className={styles.spinner} aria-hidden="true" />}
          {loading ? t("system.loading") : t("system.load")}
        </p>
      )}
    </section>
  );
}

/** 一张节卡片：色点 + 显示名 + 原始标签 + 正文（可递归一层嵌套子节）。 */
function PromptSectionCard({ section, depth }: { section: PromptSection; depth: number }) {
  const { t } = useI18n();
  const blocks = useMemo(() => toBlocks(section.body), [section.body]);
  const tone = toneOf(section);
  // 内联 AGENTS.md 时标签带 `path="…"`，把它亮出来才能分清是哪个文件。
  const attribute = section.attribute?.replace(/^path=/, "").replace(/^"|"$/g, "") ?? null;

  return (
    <article className={styles.section} data-tone={tone} data-depth={depth}>
      <header className={styles.sectionHead}>
        <span className={styles.sectionDot} aria-hidden="true" />
        <span className={styles.sectionLabel}>{t(labelKeyOf(section))}</span>
        {attribute && <span className={styles.sectionPath} title={attribute}>{attribute}</span>}
        {section.tag && <code className={styles.sectionTag}>&lt;{section.tag}&gt;</code>}
      </header>
      {blocks.length > 0 && (
        <div className={styles.sectionBody}>
          {blocks.map((block, blockIndex) => block.kind === "list" ? (
            <ul key={`list-${blockIndex}`} className={styles.list}>
              {block.lines.map((line, lineIndex) => (
                <li key={`item-${lineIndex}`} className={styles.item}>
                  <span className={styles.bullet} aria-hidden="true" />
                  <span className={styles.itemText}>{line}</span>
                </li>
              ))}
            </ul>
          ) : (
            <p key={`text-${blockIndex}`} className={styles.paragraph}>
              {block.lines.join("\n")}
            </p>
          ))}
        </div>
      )}
      {section.children.length > 0 && (
        <div className={styles.children}>
          {section.children.map((child) => (
            <PromptSectionCard key={child.id} section={child} depth={depth + 1} />
          ))}
        </div>
      )}
    </article>
  );
}
