"use client";

import { useCallback, useSyncExternalStore } from "react";
import {
  applyChatAppearance,
  DEFAULT_CHAT_APPEARANCE,
  readChatAppearance,
  writeChatAppearance,
  type ChatAppearance,
} from "@/lib/chat-appearance";

// Module-level store (mirrors hooks/useTheme.ts): the value is document-wide, so
// every consumer must observe the same snapshot instead of holding its own copy.
const listeners = new Set<() => void>();
let appearance: ChatAppearance | null = null;

function emit(): void {
  listeners.forEach((cb) => cb());
}

function ensureAppearance(): ChatAppearance {
  if (typeof window === "undefined") return DEFAULT_CHAT_APPEARANCE;
  if (appearance) return appearance;
  appearance = readChatAppearance();
  applyChatAppearance(appearance);
  return appearance;
}

function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  ensureAppearance();
  return () => {
    listeners.delete(cb);
  };
}

function getSnapshot(): ChatAppearance {
  return ensureAppearance();
}

function getServerSnapshot(): ChatAppearance {
  return DEFAULT_CHAT_APPEARANCE;
}

function setPreference(key: keyof ChatAppearance, value: number): void {
  if (typeof window === "undefined") return;
  const current = ensureAppearance();
  const next = writeChatAppearance(current, key, value);
  if (next[key] === current[key]) return;
  appearance = next;
  applyChatAppearance(next);
  emit();
}

const setWidth = (value: number) => setPreference("width", value);
const setFontSize = (value: number) => setPreference("fontSize", value);

export function useChatAppearance() {
  const snapshot = useSyncExternalStore(subscribe, getSnapshot, getServerSnapshot);
  const setChatWidth = useCallback((value: number) => setWidth(value), []);
  const setChatFontSize = useCallback((value: number) => setFontSize(value), []);
  return { width: snapshot.width, fontSize: snapshot.fontSize, setWidth: setChatWidth, setFontSize: setChatFontSize };
}
