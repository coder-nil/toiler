/**
 * 把系统提示词里的 `<project_context>` 段压缩成「文件路径 + 读取指引」。
 *
 * pi 的 `renderProjectContext()`（`@earendil-works/pi-coding-agent` 的 `core/system-prompt.js`）
 * 无条件把每个 AGENTS.md 全文拼进系统提示词：全局 `~/.pi/agent/AGENTS.md` 加上 cwd 到根的
 * 每一级祖先目录。它们随后由 `_installAgentForcedPromptProjection()` 每轮作为 `content` 发送，
 * 而 Pi Desktop 的 `createLanguagePromptExtension` 正好拥有那份 forced 全文，所以这里是唯一
 * 能改写它的地方。
 *
 * 代价与取舍：
 * - 收益是「模型没读的那几轮」真正省下整份 AGENTS.md（本项目约 90KB / 2.2 万 token）。
 * - 模型一旦 `read`，那份内容同样进上下文窗口并留在历史里，成本与内联几乎相同，只多一次往返。
 * - 压缩（compaction）会按 `firstKeptEntryId` 丢弃更早的历史，而 system 提示词每轮都会重新渲染，
 *   所以内联版永远在、按需版会在压缩后消失。摘要文本因此要求模型「压缩后重新读取」。
 */

const PROJECT_CONTEXT_BLOCK = /<project_context>[\s\S]*?<\/project_context>/;
const INSTRUCTIONS_PATH = /<project_instructions path="([^"]+)">/g;
/**
 * 当前项目路径。pi 的 `buildSystemPromptSections()` 无条件写 `<cwd>\n<path>\n</cwd>`
 * （并做反斜杠归一化），所以段内文本就是本会话的工作目录。
 */
const CWD_BLOCK = /<cwd>\s*([\s\S]*?)\s*<\/cwd>/;

export type DigestLocale = "zh-CN" | "en";

/**
 * 摘要正文：中英文各一份，与提示词其余部分一样跟随界面语言。
 *
 * `cwd` 必须写进来：`<cwd>` 段排在本段**后面**，而纯路径列表只能靠 AGENTS.md 的位置
 * 反推项目根；模型拿不到明确的「本项目在哪」时，就会跑到无关目录（甚至仓库外）翻代码。
 * 取不到 `<cwd>` 时省略这一行，其余照旧。
 */
function digestText(paths: string[], locale: DigestLocale, cwd?: string): string {
  const list = paths.map((path) => `- ${path}`).join("\n");
  if (locale === "zh-CN") {
    const root = cwd
      ? `当前项目路径：${cwd}\n本项目的代码、命令与文件都在这个目录内，不要到其它目录里去找本项目的代码。\n\n`
      : "";
    return `<project_context>
${root}本项目专属的说明与行为准则没有内联在这里，它们存放在以下文件：

${list}

这些文件里是本项目的强制约定（验证命令、禁止操作、架构约束），不是可选的背景资料。
在开始任何代码修改或命令执行之前，先用 read 工具完整读取；如果上下文被压缩过，
或者你发现自己的做法与约定可能冲突，重新读取对应文件再继续。
</project_context>`;
  }
  const root = cwd
    ? `Current project path: ${cwd}\nThis project's code, commands and files live in that directory; do not go looking for this project's code elsewhere.\n\n`
    : "";
  return `<project_context>
${root}This project's specific instructions and guidelines are not inlined here. They live in:

${list}

Those files carry binding conventions for this project (verification commands, forbidden
operations, architecture constraints), not optional background. Read them in full with the
read tool before changing code or running commands; re-read the relevant file if the
conversation was compacted or your approach may conflict with the conventions.
</project_context>`;
}

/**
 * 改写提示词中的 `<project_context>` 段。
 *
 * 保守回退（一律原样返回，宁可多花 token 也不给模型一条读不到的路径）：
 * - 没有 `<project_context>` 段；
 * - `canRead` 为 false（`read` 工具被禁用或隐藏）；
 * - 段内提取不到任何 `<project_instructions path="…">`。
 */
export function digestProjectContext(
  prompt: string,
  locale: DigestLocale,
  canRead: boolean,
): string {
  if (!canRead) return prompt;
  const block = prompt.match(PROJECT_CONTEXT_BLOCK);
  if (!block) return prompt;

  const paths = [...block[0].matchAll(INSTRUCTIONS_PATH)].map((match) => match[1]);
  if (paths.length === 0) return prompt;

  const cwd = prompt.match(CWD_BLOCK)?.[1].trim() || undefined;
  // 正则字面量带 g 标志时 lastIndex 有状态，matchAll 会自行克隆，这里无需重置。
  return prompt.replace(PROJECT_CONTEXT_BLOCK, digestText(paths, locale, cwd));
}

/** 只做检测，不改写：`<project_context>` 是否已被压缩成路径摘要。 */
export function isDigestedProjectContext(prompt: string): boolean {
  const block = prompt.match(PROJECT_CONTEXT_BLOCK);
  return block !== null && !/<project_instructions path=/.test(block[0]);
}
