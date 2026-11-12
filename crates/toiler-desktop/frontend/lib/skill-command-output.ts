import { stripAnsi } from "./ansi";

const TERMINAL_FRAME_RE = /\x1B\[1G\x1B\[J/g;
// spinner 帧的行首带一个旋转字形（◐ ◓ ◑ ◒），原正则锚在行首所以一直漏掉。
const SPINNER_GLYPH_RE = /^[\u25D0-\u25D3\u2596-\u259F]+\s*/;
const SPINNER_TEXT_RE = /^(?:Parsing source|Cloning repository|Downloading source|Discovering skills|Installing skills)[.\u2026]*$/;

// skills CLI 会先打一个 FIGlet 大字 banner（`skills`），每行都由 █ ╗ ╔ ╝ ╚ ║ ═ 这类方块/线框
// 字形拼成。现有 SPINNER_LINE_RE 只过滤 spinner 文字，于是超时被杀的输出里剩下十几行
// banner 字符，用户看到的就是一串无法阅读的方块。带 -y 运行时它没有信息量，直接丢掉。
const ASCII_ART_GLYPH_RE = /[\u2580-\u259F\u2500-\u257F]/;

/** 纯由 banner 字符、空格和边框组成的行（例如 `██╔══╝`、`┌   skills`）。 */
function isAsciiArtLine(line: string): boolean {
  if (!ASCII_ART_GLYPH_RE.test(line)) return false;
  // 只要出现字母数字就当作真内容，避免误删含命令输出的行。
  return !/[\p{L}\p{N}]/u.test(line.replace(/[\u2580-\u259F\u2500-\u257F]/g, ""));
}

/** 动画帧（旋转变动的 spinner），包括带旋转字形的 `◒  Cloning repository…`。 */
function isSpinnerLine(line: string): boolean {
  return SPINNER_TEXT_RE.test(line.trim().replace(SPINNER_GLYPH_RE, ""));
}

/** Convert the skills CLI's animated terminal output into useful API text. */
export function formatSkillCommandOutput(raw: string): string {
  const lines = stripAnsi(raw.replace(TERMINAL_FRAME_RE, "\n"))
    .replace(/\r/g, "\n")
    .split("\n")
    .map((line) => line.trim())
    .filter((line) => line && !isSpinnerLine(line) && !isAsciiArtLine(line));

  return lines.filter((line, index) => line !== lines[index - 1]).join("\n").slice(-4_000);
}
