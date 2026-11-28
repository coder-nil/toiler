export function normalizeMergeText(text: string): string {
  return text.replace(/\r\n?/g, "\n");
}

export function mergeTextForSave(text: string, original: string | null): string {
  if (original === null) return text;
  if (text === normalizeMergeText(original)) return original;
  // 编辑器统一用 LF；编辑过的纯 CRLF 文件仍按原格式保存。
  return original.includes("\r\n") && !/[\r\n]/.test(original.replaceAll("\r\n", "")) ? text.replaceAll("\n", "\r\n") : text;
}

export type MergeSide = "local" | "incoming";
export type MergePane = MergeSide | "result";
type Marker = { kind: string; line: number; offset: number; next: number; label: string };
export interface MergeBlock {
  startLine: number;
  endLine: number;
  from: number;
  to: number;
  local: string;
  incoming: string;
  base: string | null;
  markers: { open: string; base: string; separator: string; close: string };
  incomingLabel: string;
}

export function parseMergeDraft(text: string, size: number) {
  const markers: Marker[] = [];
  let offset = 0;
  for (const [index, line] of text.split("\n").entries()) {
    const marker = /^([<=>|])\1*/.exec(line)?.[0];
    const next = Math.min(text.length, offset + line.length + 1);
    if (marker?.length === size && (line.length === size || line[size] === " ")) {
      markers.push({ kind: marker[0], line: index + 1, offset, next, label: line.slice(size).trim() });
    }
    offset = next;
  }
  const blocks: MergeBlock[] = [];
  for (let i = 0; i < markers.length;) {
    if (markers[i].kind !== "<") { i++; continue; }
    const start = i++;
    let depth = 1;
    while (i < markers.length && depth > 0) {
      if (markers[i].kind === "<") depth++;
      if (markers[i].kind === ">") depth--;
      i++;
    }
    // 不完整、嵌套或损坏的标记只允许手动处理，不能猜测要替换的范围。
    const count = i - start;
    if (depth || (count !== 3 && count !== 4)) continue;
    const [open, first, second, third] = markers.slice(start, i);
    const [separator, close] = third ? [second, third] : [first, second];
    if ((third && first.kind !== "|") || separator.kind !== "=" || close.kind !== ">") continue;
    blocks.push({ startLine: open.line, endLine: close.line, from: open.offset, to: close.next,
      local: text.slice(open.next, (third ? first : separator).offset),
      incoming: text.slice(separator.next, close.offset),
      base: third ? text.slice(first.next, separator.offset) : null,
      markers: { open: text.slice(open.offset, open.next), base: third ? text.slice(first.offset, first.next) : "",
        separator: text.slice(separator.offset, separator.next), close: text.slice(close.offset, close.next) },
      incomingLabel: close.label });
  }
  return { markers, blocks, looseMarkers: markers.filter((marker) => {
    // 块有序；用二分定位标记所属的上一块，避免大文件退化为逐块扫描。
    let lo = 0, hi = blocks.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (blocks[mid].startLine <= marker.line) lo = mid + 1;
      else hi = mid;
    }
    return lo === 0 || marker.line > blocks[lo - 1].endLine;
  }) };
}

export function mergeMarkerOffsets(text: string, size: number): number[] {
  return parseMergeDraft(text, size).markers.map((marker) => marker.offset);
}

export function mergeBlockText(block: MergeBlock, side: MergeSide, draft: string, source: string) {
  const choice = block[side];
  // Git 会在 EOF 的标记之前补一个 LF，采纳原本无末尾换行的版本时还原它。
  const withoutLF = choice.slice(0, -1);
  return choice.endsWith("\n") && block.to === draft.length &&
    (source === withoutLF || source.endsWith(`\n${withoutLF}`)) ? withoutLF : choice;
}

interface Match { rows: (number | null)[]; before: Map<number, number[]> }

function matchLines(ref: string[], source: string[]): Match {
  const rows: Match["rows"] = [];
  const before: Match["before"] = new Map();
  let prefix = 0, end = ref.length, sourceEnd = source.length;
  while (prefix < end && prefix < sourceEnd && ref[prefix] === source[prefix]) prefix++;
  while (end > prefix && sourceEnd > prefix && ref[end - 1] === source[sourceEnd - 1]) { end--; sourceEnd--; }
  // 选出两侧唯一且单调递增的锚点。仅影响视觉对齐，采纳操作始终使用 Git 的标记范围。
  function positions(lines: string[], stop: number) {
    const unique = new Map<string, number>();
    for (let i = prefix; i < stop; i++) unique.set(lines[i], unique.has(lines[i]) ? -1 : i);
    return unique;
  }
  const refPositions = positions(ref, end);
  const sourcePositions = positions(source, sourceEnd);
  const tails: { ref: number; source: number; prev: number }[] = [];
  const choices: number[] = [];
  for (let i = prefix; i < end; i++) {
    const at = sourcePositions.get(ref[i]) ?? -1;
    if (at < 0 || refPositions.get(ref[i]) !== i) continue;
    let lo = 0, hi = choices.length;
    while (lo < hi) {
      const mid = (lo + hi) >>> 1;
      if (tails[choices[mid]].source < at) lo = mid + 1;
      else hi = mid;
    }
    choices[lo] = tails.push({ ref: i, source: at, prev: lo ? choices[lo - 1] : -1 }) - 1;
  }
  const anchors = [{ ref: end, source: sourceEnd }];
  for (let i = choices.at(-1) ?? -1; i >= 0; i = tails[i].prev) anchors.push(tails[i]);
  for (let i = 0; i < prefix; i++) rows.push(i + 1);
  let a = prefix, b = prefix;
  for (const anchor of anchors.reverse()) {
    while (a < anchor.ref) rows[a++] = b < anchor.source ? ++b : null;
    if (b < anchor.source) before.set(a, Array.from({ length: anchor.source - b }, (_, i) => b + i + 1));
    b = anchor.source;
    if (a < end) rows[a++] = ++b;
  }
  while (a < ref.length) rows[a++] = ++b;
  return { rows, before };
}

export type MergeRow = [number | null, number | null, number | null];

export function alignMergeRows(result: string[], local: string[], incoming: string[], blocks: MergeBlock[]) {
  // 避免极端多行文本创建大量 Monaco 视图占位；不影响冲突操作和保存。
  if (result.length + local.length + incoming.length > 60_000) return null;
  const chunks: { from: number; to: number; lengths: [number, number]; block?: MergeBlock }[] = [];
  const projected: [string[], string[]] = [[], []];
  let line = 1;
  function common(to: number) {
    const value = result[line - 1];
    projected[0].push(value);
    projected[1].push(value);
    chunks.push({ from: line, to, lengths: [1, 1] });
    line = to + 1;
  }
  for (const block of blocks) {
    while (line < block.startLine) common(line);
    const sides = [block.local, block.incoming].map((value) => value ? value.slice(0, -1).split("\n") : []);
    for (const [index, lines] of sides.entries()) projected[index].push(...lines);
    chunks.push({ from: line, to: block.endLine, lengths: [sides[0].length, sides[1].length], block });
    line = block.endLine + 1;
  }
  while (line <= result.length) common(line);

  const matches = [matchLines(projected[0], local), matchLines(projected[1], incoming)];
  const at = [0, 0];
  const rows: MergeRow[] = [];
  const conflictRows = new Map<number, number>();
  function append(left: (number | null)[], right: (number | null)[], from = 0, count = 0) {
    for (let i = 0, length = Math.max(left.length, right.length, count); i < length; i++) {
      rows.push([left[i] ?? null, i < count ? from + i : null, right[i] ?? null]);
    }
  }
  for (const { from, to, lengths, block } of chunks) {
    const extras = matches.map((match, i) => {
      const lines = match.before.get(at[i]) ?? [];
      match.before.delete(at[i]);
      return lines;
    });
    append(extras[0], extras[1]);
    if (block) conflictRows.set(from, rows.length);
    const group = matches.map((match, side) => {
      const lines = [];
      for (let i = 0; i < lengths[side]; i++, at[side]++) {
        for (const extra of match.before.get(at[side]) ?? []) lines.push(extra);
        lines.push(match.rows[at[side]]);
      }
      return lines;
    });
    append(group[0], group[1], from, to - from + 1);
  }
  append(matches[0].before.get(at[0]) ?? [], matches[1].before.get(at[1]) ?? []);
  return { rows, conflictRows };
}

export function changedMergeColumns(text: string, other: string): [number, number] {
  let start = 0, end = text.length, otherEnd = other.length;
  while (start < end && start < otherEnd && text[start] === other[start]) start++;
  while (end > start && otherEnd > start && text[end - 1] === other[otherEnd - 1]) { end--; otherEnd--; }
  return [start + 1, end + 1];
}
