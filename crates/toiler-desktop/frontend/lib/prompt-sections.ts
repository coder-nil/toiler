/**
 * 把系统提示词按后端的分节结构切开，供「系统提示词」面板做结构化展示。
 *
 * # 为什么不用 Markdown 渲染
 *
 * 区块边界就是 `<tools>` / `<rules>` / `<cwd>` 这些 XML 式标签。走
 * react-markdown + rehypeRaw 的话，它们会被当成真正的 HTML 元素：浏览器不
 * 认识 `<tools>`，于是标签本身在视图里消失，只剩内容无归属地堆在一起 ——
 * 想「按标签分色块」与「Markdown 渲染」这两件事在提示词上是互斥的。
 *
 * 另外 `<rules>` 里的 `edits[].oldText`、内联 AGENTS.md 里的 `#` 与 `**` 都会
 * 被 Markdown 改写。而面板的存在意义就是「看到的等于实际发出去的」，所以这里
 * 只做结构切分，**不改写任何正文字节**。
 *
 * # 切分依据
 *
 * `crates/toiler-server/src/system_prompt.rs` 的 `compose()` 恒定产出：
 * 裸 preamble + 若干个独占整行的 `<tag>…</tag>` 块 + 裸的语言指令。
 * 只认 `KNOWN_TAGS` 白名单：内联 AGENTS.md 正文里出现 `<div>` 之类时按普通
 * 文本处理，绝不会被误认成新的一节。
 */

/** 节标签名。由后端组装，新增标签要同时加进这里才有颜色与标题。 */
const KNOWN_TAGS = new Set([
  "tools",
  "rules",
  "project_context",
  "project_instructions",
  "cwd",
  "env",
  "skills",
  "docs",
]);

/** `<tools>` / `<project_instructions path="…">`；属性里的 `>` 已被后端转义成 `&gt;`。 */
const OPEN_TAG = /^<([a-z][a-z0-9_]*)(\s[^<>]*)?>$/;
/** `</tools>` */
const CLOSE_TAG = /^<\/([a-z][a-z0-9_]*)>$/;

/** 拆出来的原始块，还没有递归处理嵌套。 */
interface FlatSection {
  tag: string | null;
  attribute: string | null;
  body: string;
}

export type PromptSectionRole = "tagged" | "preamble" | "epilogue" | "note";

export interface PromptSection {
  /** React key。同一标签在文本里出现多次时靠序号区分。 */
  id: string;
  /** 节标签名；裸文本段为 `null`。 */
  tag: string | null;
  /** 开标签的属性原文（`<project_instructions path="…">` 里的 `path="…"`）。 */
  attribute: string | null;
  /** 节正文里**不属于任何子节**的部分，首尾空行已去掉，行首缩进一字不动。 */
  body: string;
  /**
   * 裸文本段的语义只能靠位置推断：顶层的第一段是 preamble（概述），其余是追加在
   * 末尾的语言指令；嵌在区块里的裸文本是那个区块的引导语（note）。
   * 这与 `compose()` 的拼装顺序一一对应。
   */
  role: PromptSectionRole;
  /**
   * 嵌套的子节。只有没有 `read` 工具时才会出现：那时后端把 AGENTS.md 全文
   * 内联成 `<project_context>` 里的一层层 `<project_instructions path="…">`。
   */
  children: PromptSection[];
}

/** 只删首尾的空行，不动行内缩进 —— `pre-wrap` 的排面靠这些缩进。 */
function trimBlankEdges(lines: string[]): string[] {
  let start = 0;
  let end = lines.length;
  while (start < end && lines[start].trim() === "") start += 1;
  while (end > start && lines[end - 1].trim() === "") end -= 1;
  return lines.slice(start, end);
}

/**
 * 平铺切分：只认顶层标签，不递归。
 *
 * 不收尾未闭合的标签（遇到 `</tag>` 才算一节结束）：宁可让它退回普通文本，
 * 也不要把后面的内容整段吞进一个不存在的区块。
 */
function splitFlat(prompt: string): FlatSection[] {
  const out: FlatSection[] = [];
  let buffer: string[] = [];
  let open: { tag: string; attribute: string | null } | null = null;

  const flush = (tag: string | null, attribute: string | null) => {
    const body = trimBlankEdges(buffer).join("\n");
    buffer = [];
    // 空白裸段（节与节之间的空行）不产卡片。
    if (tag === null && body === "") return;
    out.push({ tag, attribute, body });
  };

  for (const line of prompt.split("\n")) {
    if (open === null) {
      const opening = OPEN_TAG.exec(line);
      const name = opening?.[1];
      if (opening !== null && name !== undefined && KNOWN_TAGS.has(name)) {
        flush(null, null);
        open = { tag: name, attribute: opening[2]?.trim() || null };
        continue;
      }
      buffer.push(line);
      continue;
    }
    const closing = CLOSE_TAG.exec(line);
    if (closing?.[1] === open.tag) {
      flush(open.tag, open.attribute);
      open = null;
      continue;
    }
    buffer.push(line);
  }
  // 未闭合：把已读到的内容当普通文本交回去（含开标签那一行看不到，这里补不回来，
  // 但这是畸形容错路径，真正的原文永远有「原文」视图可查）。
  flush(open?.tag ?? null, open?.attribute ?? null);
  return out;
}

/** 给平铺结果补上 id / role / 嵌套子节。 */
function assemble(flat: FlatSection[], depth: number): PromptSection[] {
  let plainCount = 0;
  return flat.map((item, index) => {
    const children = item.tag === null ? [] : assembleChildren(item.body, depth + 1);
    const role: PromptSectionRole = item.tag !== null
      ? "tagged"
      : depth > 0
        // 区块内的散文（「项目专属说明和行为准则：」）只是那个区块的引导语，
        // 不能也叫它「角色」——那是顶层 preamble 的名字。
        ? "note"
        : (plainCount++ === 0 ? "preamble" : "epilogue");
    return {
      id: `${item.tag ?? "plain"}-${depth}-${index}`,
      tag: item.tag,
      attribute: item.attribute,
      // 有子节时，正文已经整段交给子节（引导语也变成了一个 note 子节），
      // 父节再留一份会把同一段话渲染两遍。
      body: children.length === 0 ? item.body : "",
      role,
      children,
    };
  });
}

/** 递归切一层嵌套。切不出标签就返回空数组，父节照旧显示自己的整段正文。 */
function assembleChildren(body: string, depth: number): PromptSection[] {
  const inner = splitFlat(body);
  if (!inner.some((item) => item.tag !== null)) return [];
  return assemble(inner, depth);
}

/**
 * 按标签把提示词切成有序的节。
 *
 * @param prompt 后端 `get_state.systemPrompt` 的原文
 * @returns 切好的节。切不出任何标签时返回单个裸段，调用方据此隐藏视图切换
 */
export function splitPromptSections(prompt: string): PromptSection[] {
  const flat = splitFlat(prompt);
  // 兜底：切出来是空的也要有一张卡片，否则面板会空白。
  if (flat.length === 0) {
    return prompt === ""
      ? []
      : [{ id: "plain-0-0", tag: null, attribute: null, body: prompt, role: "preamble", children: [] }];
  }
  return assemble(flat, 0);
}

/** 文本里是否真的存在可分的节。没有就不值得显示「分节 / 原文」开关。 */
export function hasTaggedSections(sections: PromptSection[]): boolean {
  return sections.some((section) => section.tag !== null || section.children.length > 0);
}
