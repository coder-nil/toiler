import { parseMergeDraft, type MergeBlock } from "./git-conflicts";

export type SimpleMergePlans = ReadonlyMap<string, string>;

function positions(lines: string[]) {
  const indices = new Map<string, number>();
  for (const [index, line] of lines.entries()) indices.set(line, indices.has(line) ? -1 : index);
  return indices;
}

function mergeBlock(block: MergeBlock, common: string): string | undefined {
  const lines = (value: string) => value ? value.slice(0, -1).split("\n") : [];
  const local = lines(block.local), base = lines(common), incoming = lines(block.incoming);
  if (base.length !== local.length || base.length !== incoming.length) return;
  const basePositions = positions(base);
  const sides = [local, incoming];
  // 只处理能按祖先行一一对齐的简单替换。插删、移行和重复行导致的歧义不猜。
  if (sides.some((side) => side.some((line, i) => line !== base[i] &&
    (basePositions.get(base[i]) !== i || basePositions.has(line))))) return;

  let result = "", applied = false, start = -1;
  function flush(end: number) {
    if (start < 0) return;
    const slice = (lines: string[]) => `${lines.slice(start, end).join("\n")}\n`;
    result += block.markers.open + slice(local) +
      (block.base === null ? "" : block.markers.base + slice(base)) +
      block.markers.separator + slice(incoming) + block.markers.close;
    start = -1;
  }
  for (const [i, ancestor] of base.entries()) {
    const a = local[i], b = incoming[i];
    if (a !== b && a !== ancestor && b !== ancestor) {
      if (start < 0) start = i;
      continue;
    }
    flush(i);
    result += `${a === ancestor ? b : a}\n`;
    applied = true;
  }
  flush(base.length);
  return applied ? result : undefined;
}

export function planSimpleMerge(original: string, diff3: string, size: number, canMergeEof: boolean): SimpleMergePlans {
  const draft = parseMergeDraft(original, size);
  const replay = parseMergeDraft(diff3, size);
  if (draft.looseMarkers.length || replay.looseMarkers.length || draft.blocks.length !== replay.blocks.length) return new Map();
  // 只在 Git 重放出来的块边界和内容都能与工作树配对时使用祖先，不能按行号猜。
  if (draft.blocks.some((block, i) => {
    const candidate = replay.blocks[i];
    return candidate.base === null || block.local !== candidate.local || block.incoming !== candidate.incoming ||
      (block.base !== null && block.base !== candidate.base);
  })) return new Map();
  const keys = draft.blocks.map(({ local, incoming }) => JSON.stringify([local, incoming]));
  const unique = positions(keys);
  const plans = new Map<string, string>();
  for (const [i, block] of draft.blocks.entries()) {
    if (unique.get(keys[i]) !== i || !block.markers.close.endsWith("\n") || (block.to === original.length && !canMergeEof)) continue;
    const replacement = mergeBlock(block, replay.blocks[i].base!);
    if (replacement !== undefined) plans.set(original.slice(block.from, block.to), replacement);
  }
  return plans;
}

export function simpleMergeEdits(text: string, blocks: MergeBlock[], plans: SimpleMergePlans) {
  if (!plans.size) return [];
  const originals = blocks.map(({ from, to }) => text.slice(from, to));
  const unique = positions(originals);
  return blocks.flatMap(({ from, to }, i) => {
    const original = originals[i];
    const replacement = plans.get(original);
    // 编辑中的块、复制出的同样标记以及已解决的块都不能被批量操作覆盖。
    return replacement !== undefined && unique.get(original) === i ? [{ from, to, text: replacement }] : [];
  });
}
