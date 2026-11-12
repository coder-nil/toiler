/**
 * 输入框 π 图标切换的对话模式偏好（浏览器持久化）。
 *
 * 与工具预设偏好（`lib/tool-preset-preference.ts`）不同：模式是**全局**的开关。
 * 普通对话模式不属于会话内容，不写进 `.jsonl`；它决定新会话如何创建，以及打开任意
 * 会话时是否把该会话也切到普通对话（服务端 `set_chat_mode`）。
 */

const STORAGE_KEY = "pi-chat-mode";

export type ChatMode = "work" | "chat";

interface StorageLike {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}

function getBrowserStorage(): StorageLike | null {
  if (typeof window === "undefined") return null;
  try {
    return window.localStorage;
  } catch {
    return null;
  }
}

export function getPreferredChatMode(storage: StorageLike | null = getBrowserStorage()): ChatMode {
  if (!storage) return "work";
  try {
    return storage.getItem(STORAGE_KEY) === "chat" ? "chat" : "work";
  } catch {
    return "work";
  }
}

export function setPreferredChatMode(
  mode: ChatMode,
  storage: StorageLike | null = getBrowserStorage(),
): void {
  if (!storage) return;
  try {
    storage.setItem(STORAGE_KEY, mode);
  } catch {
    // Browser storage is best-effort.
  }
}
