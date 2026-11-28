export function normalizeMergeText(text: string): string {
  return text.replace(/\r\n?/g, "\n");
}

export function mergeTextForSave(text: string, original: string | null): string {
  if (original === null) return text;
  if (text === normalizeMergeText(original)) return original;
  // textarea 总用 LF；编辑过的纯 CRLF 文件仍按原格式保存。
  return original.includes("\r\n") && !/[\r\n]/.test(original.replaceAll("\r\n", "")) ? text.replaceAll("\n", "\r\n") : text;
}

export function mergeMarkerOffsets(text: string, size: number): number[] {
  const offsets: number[] = [];
  let offset = 0;
  for (const line of text.split("\n")) {
    const marker = /^[<=>|]+/.exec(line)?.[0];
    if (marker?.length === size && [...marker].every((part) => part === marker[0]) && (line.length === size || line[size] === " ")) {
      offsets.push(offset);
    }
    offset += line.length + 1;
  }
  return offsets;
}
