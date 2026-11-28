export type EditorHistoryAction = "undo" | "redo";

// macOS 的菜单动作不一定会派发 keydown；只有当前编辑器接管时才阻止 WebView 默认撤销。
export function listenForNativeEditorHistory(perform: (action: EditorHistoryAction) => boolean): () => void {
  const handle = (event: Event) => {
    const action: unknown = (event as CustomEvent<unknown>).detail;
    if (!event.defaultPrevented && (action === "undo" || action === "redo") && perform(action)) {
      event.preventDefault();
    }
  };
  window.addEventListener("toiler:editor-history", handle);
  return () => window.removeEventListener("toiler:editor-history", handle);
}
