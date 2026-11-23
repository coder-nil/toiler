import { getCodeLanguageIcon } from "./FileIcons";

/**
 * 代码块头部左侧的语言标签。
 *
 * 单独抽成一个组件，是为了让桌面（`MermaidBlock.tsx`，拉 prism 高亮）和手机
 * （`MobileMarkdown.tsx`，有意不带语法高亮）共用同一处渲染，而不必让手机端去
 * import 桌面那一坨依赖。这里只依赖 `FileIcons` 的 SVG 图标，没有额外包体。
 */
export function MarkdownCodeLang({ language }: { language: string }) {
  const label = language || "text";
  const icon = getCodeLanguageIcon(label, 13);

  return (
    <span className="markdown-code-lang">
      {icon}
      {label}
    </span>
  );
}
