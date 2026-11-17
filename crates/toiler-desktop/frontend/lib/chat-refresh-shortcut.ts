type RefreshKeyEvent = Pick<KeyboardEvent,
  "key" | "code" | "metaKey" | "ctrlKey" | "altKey" | "shiftKey" | "isComposing" | "repeat" | "preventDefault"
>;

export function isChatRefreshShortcut(event: RefreshKeyEvent): boolean {
  if (event.isComposing || event.altKey || event.shiftKey) return false;
  const plainF5 = event.key === "F5" && !event.metaKey && !event.ctrlKey;
  const primaryR = (event.metaKey || event.ctrlKey)
    && (event.code === "KeyR" || event.key.toLowerCase() === "r");
  return plainF5 || primaryR;
}

// 捕获阶段拦截原生重载，保留输入草稿和正在进行的 SSE；连按不叠加请求。
export function createChatRefreshHandler(refresh: () => Promise<unknown>) {
  let pending = false;
  return async (event: RefreshKeyEvent): Promise<void> => {
    if (!isChatRefreshShortcut(event)) return;
    event.preventDefault();
    if (event.repeat || pending) return;
    pending = true;
    try {
      await refresh();
    } finally {
      pending = false;
    }
  };
}
