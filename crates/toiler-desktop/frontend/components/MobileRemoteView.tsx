"use client";

import { useCallback, useEffect, useMemo, useReducer, useRef, useState } from "react";
import { ArrowDown, ArrowUp, Plus } from "lucide-react";
import { useI18n } from "@/hooks/useI18n";
import { useViewportHeight } from "@/hooks/useViewportHeight";
import { INITIAL_STREAMING_STATE, streamReducer } from "@/lib/streaming-message";
import { projectAssistantBlocks, type MobileMessage } from "@/lib/mobile-state";
import { MOBILE_DEFAULT_LIMIT, MOBILE_LIMIT_STEP, MOBILE_MAX_LIMIT } from "@/lib/mobile-state";
import { buildMobileTimeline, MOBILE_RADIUS, reconcilePendingUserMessages } from "@/lib/mobile-timeline";
import { MobileMarkdown } from "./MobileMarkdown";
import { MobileProcessDetails } from "./MobileProcessDetails";
import { MobileThinking, MobileToolList } from "./MobileProcessParts";
import { MobilePromptCard, type MobileUiRequest } from "./MobilePromptCard";

/**
 * 手机遥控页：同一份会话数据的精简视图。
 *
 * 数据流：
 *   1. `GET /api/mobile/state` 取一次快照（消息已在服务端压成纯文本）；
 *   2. `GET /api/agent/[id]/events` SSE 推送流式进度，只用于渲染「正在输出」的尾巴；
 *   3. prompt / abort / 新建任务分别复用桌面端的 `POST /api/agent/[id]` 与
 *      `POST /api/agent/new`，不新增写接口。
 *
 * 视觉约束（一屏内的规则，改动时请一起遵守）：
 *   - 圆角只有四档：6 小标签 / 12 输入容器 / 14 消息气泡 / 999 头部与主操作按钮；
 *   - 次级文字一律 `--text-muted`，`--text-dim` 在浅色底上达不到 AA 对比度；
 *   - 状态色只用 `--live` / `--danger`，它们在两套主题里都够深/够亮；
 *   - 只有被测量过的动效点才配 class：输出光标、等待点、骨架、状态脉冲、按压缩放；
 *   - 一轮里的过程（思考 + 中间正文 + 工具调用）折进 MobileProcessDetails：
 *     传 running 的那一轮在跑时展开能看着它长，跑完自动收起；最终回答留在组外；
 *   - 助手正文走 MobileMarkdown（轻量 markdown），用户消息保持原文 pre-wrap：
 *     用户粘贴的日志/代码不应该被当成 markdown 改写。
 *
 * 与桌面端的同步：手机端只有这一条 SSE，快照不会自己更新，所以几张「该补快照」的
 * 信号全部都接上了——`agent_start` / `queue_update`（桌面端发起的提问要马上出现）、
 * 重连与 `online`（断线期间的事件不会重放）、运行中静默超时（半开连接）。
 */

/** 运行中多久没收到任何事件，就补一次快照（半开连接不会触发 onerror）。 */
const IDLE_STREAM_REFRESH_MS = 20_000;
/** 上面那个静默检查的轮询间隔。 */
const STREAM_WATCHDOG_TICK_MS = 5_000;
/**
 * 输入框自增长的上限：16px 字号 × 1.5 行高 = 24px/行，含上下内边距 16px，
 * 88px 正好 3 行。手机键盘顶起后可视区很小，再高就把转录区挤没了，超出的文本在框内滚动。
 */
const COMPOSER_MAX_HEIGHT = 88;

/** 距底部小于这个距离就认为「跟着最新内容走」。 */
const STICK_THRESHOLD_PX = 56;

interface MobileStateSnapshot {
  sessionId: string | null;
  name: string | null;
  cwd: string | null;
  status: "running" | "idle";
  updatedAt: string | null;
  queue: { steering: number; followUp: number };
  messages: MobileMessage[];
  /** 窗口之外还有多少条更早的消息；翻完或服务端老版本没这个字段时当作 0。 */
  earlierCount?: number;
  /** 正在等用户回答的扩展请求（`ask_user` 就是这条路径）；没有时为 null。 */
  pendingUiRequest?: MobileUiRequest | null;
}

function formatElapsed(seconds: number): string {
  const minutes = Math.floor(seconds / 60);
  return `${minutes}:${String(seconds % 60).padStart(2, "0")}`;
}

/** 与 `lib/mobile-state.ts` 的快照时间格式一致（HH:MM）。 */
function formatClock(date: Date): string {
  return `${String(date.getHours()).padStart(2, "0")}:${String(date.getMinutes()).padStart(2, "0")}`;
}

function readParams(): { session: string | null; cwd: string | null } {
  if (typeof window === "undefined") return { session: null, cwd: null };
  const params = new URLSearchParams(window.location.search);
  return { session: params.get("session"), cwd: params.get("cwd") };
}

export type MobileEventAction = "start" | "snapshot" | "prompt" | "delta" | "settle" | "sync" | "ignore";

/** SSE 上我们关心的字段。 */
interface MobileStreamPayload {
  type?: string;
  assistantMessageEvent?: unknown;
  /** `message_start` 携带的消息（用户提问或助手的流式消息）。 */
  message?: unknown;
  /** `connected` 携带的当前状态。 */
  isStreaming?: unknown;
  /** `extension_ui_resolved` 携带的请求 id。 */
  id?: unknown;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}

/** 阻塞式扩展 UI 方法（与 `app/api/mobile/state/route.ts` 的 BLOCKING_UI_METHODS 一致）。 */
const BLOCKING_UI_METHODS = new Set(["select", "confirm", "input", "editor", "custom"]);

/**
 * 从 `extension_ui_request` 事件里取出要弹的请求。
 *
 * 事件本身就是完整请求体，所以手机端可以直接拿它渲染，不必等下一次快照（省一个往返，
 * 而且快照取失败时卡片仍然在）。`notify` / `setStatus` / `setWidget` / `setTitle` 这些
 * 不是问题，不进卡片。
 */
export function blockingUiRequestFromEvent(payload: MobileStreamPayload): MobileUiRequest | null {
  const request = payload as unknown as MobileUiRequest;
  if (typeof request.id !== "string" || !request.id) return null;
  if (typeof request.method !== "string" || !BLOCKING_UI_METHODS.has(request.method)) return null;
  if (typeof request.title !== "string") return null;
  return request;
}

/**
 * SSE 事件 → 手机端要做的动作。
 *
 * 手机端只有这一条连接、快照不会自己更新，所以「哪些事件必须换一次快照」就是同步规则的
 * 全部：`agent_start`（这一轮是桌面端 / CLI / 扩展开始的）与 `queue_update`（对面又发了
 * 一条）。抽成纯函数是为了让这条规则能被单测钉住，而不是埋在 effect 里。
 * `start` 在调用处同时意味着「开计时 + 补快照」。
 *
 * `message_start` 不是可选项：流式气泡靠它建立（`streamReducer` 在没有 `streamingMessage`
 * 时会丢弃每一条 delta），少这一步手机上就完全看不到流式输出。
 */
export function classifyAgentEvent(payload: MobileStreamPayload): MobileEventAction {
  switch (payload.type) {
    case "agent_start":
      return "start";
    case "connected":
      // 接上时服务端会告知这一轮是否还在跑；随后的 message_start 会给流式快照。
      return payload.isStreaming === true ? "start" : "ignore";
    case "message_start":
      // 用户提问那条不需要（本地气泡/快照自己管），只有助手消息才是流式气泡的起点。
      return isRecord(payload.message) && payload.message.role === "assistant" ? "snapshot" : "ignore";
    case "queue_update":
      return "sync";
    case "extension_ui_request":
      // 扩展（含 `ask_user`）在等回答：事件里就带着请求体，直接弹卡片。
      return blockingUiRequestFromEvent(payload) ? "prompt" : "ignore";
    case "extension_ui_resolved":
      // 另一边已经答了（或超时/中止），本端把卡片收掉。
      return "sync";
    case "message_update":
      return payload.assistantMessageEvent ? "delta" : "ignore";
    case "prompt_done":
    case "agent_settled":
      return "settle";
    default:
      return "ignore";
  }
}

/** 首屏骨架：形状和真实消息一致（右侧一条用户气泡 + 左侧几行正文）。 */
function TranscriptSkeleton({ label }: { label: string }) {
  const bar = { background: "var(--bg-panel)" } as const;
  return (
    <div role="status" aria-busy="true" aria-label={label} style={{ display: "flex", flexDirection: "column", gap: 18, paddingTop: 2 }}>
      <div aria-hidden="true" style={{ display: "flex", justifyContent: "flex-end" }}>
        <div className="mobile-skeleton" style={{ ...bar, width: "58%", height: 40, borderRadius: MOBILE_RADIUS.bubble }} />
      </div>
      <div aria-hidden="true" style={{ display: "flex", flexDirection: "column", gap: 9 }}>
        <div className="mobile-skeleton" style={{ ...bar, width: "92%", height: 12, borderRadius: MOBILE_RADIUS.chip }} />
        <div className="mobile-skeleton" style={{ ...bar, width: "78%", height: 12, borderRadius: MOBILE_RADIUS.chip }} />
        <div className="mobile-skeleton" style={{ ...bar, width: "46%", height: 12, borderRadius: MOBILE_RADIUS.chip }} />
      </div>
    </div>
  );
}

/** 已发出请求但还没有第一段文字：三点呼吸，表示这轮还活着。 */
function TypingPulse() {
  return (
    <div aria-hidden="true" style={{ display: "flex", alignItems: "center", gap: 5, padding: "2px 0 2px 1px" }}>
      {[0, 1, 2].map((index) => (
        <span
          key={index}
          className="mobile-typing-dot"
          style={{ width: 5, height: 5, borderRadius: "50%", background: "var(--text-muted)", animationDelay: `${index * 140}ms` }}
        />
      ))}
    </div>
  );
}

export function MobileRemoteView() {
  const { t } = useI18n();
  useViewportHeight();

  // URL 是唯一真相：读出参数之前不发任何请求，否则会用「服务端挑中的会话」覆盖扫码指定的会话。
  const [target, setTarget] = useState<{ session: string | null; cwd: string | null } | null>(null);
  const [snapshot, setSnapshot] = useState<MobileStateSnapshot | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [input, setInput] = useState("");
  const [busy, setBusy] = useState(false);
  const [elapsed, setElapsed] = useState(0);
  const [composerFocused, setComposerFocused] = useState(false);
  const [showJump, setShowJump] = useState(false);
  // 快照窗口：默认最新 30 条，点「载入更早」一批批往回长（上限与服务端一致）。
  const [limit, setLimit] = useState(MOBILE_DEFAULT_LIMIT);
  const [loadingEarlier, setLoadingEarlier] = useState(false);
  // 正在提交的扩展回答：防止连点两次把同一个请求答两遍。
  const [answeringPrompt, setAnsweringPrompt] = useState(false);
  // 事件里直接带来的待回答请求：收到就能弹，不用等快照那一跳。
  const [eventPrompt, setEventPrompt] = useState<MobileUiRequest | null>(null);
  // 刚发出去、快照里还没有的用户消息：先本地显示，避免“说完了等一轮才看到自己那句话”。
  const [pending, setPending] = useState<MobileMessage[]>([]);
  const [streaming, dispatch] = useReducer(streamReducer, INITIAL_STREAMING_STATE);
  const scrollRef = useRef<HTMLDivElement>(null);
  const composerRef = useRef<HTMLTextAreaElement>(null);
  const startedAtRef = useRef<number | null>(null);
  // 上一次快照里已经见过的条目 id：只有新出现的条目才能接管本地气泡。
  const seenMessageIdsRef = useRef<Set<string>>(new Set());
  const pendingIdRef = useRef(0);
  // 最近一次收到 SSE 事件的时间：给「运行中静默超时」那个看门狗用。
  const lastEventAtRef = useRef<number | null>(null);
  // 本轮还差一次「拿到提问正文」的快照（见 agent_start 那一支的注释）。
  const syncOnFirstDeltaRef = useRef(false);
  // 上一次渲染后的布局快照：窗口头部变了（往回加载，或被新消息挤掉最旧的一条）
  // 就靠它把视图钉在原地，否则用户正在看的历史会往上跑。
  const lastLayoutRef = useRef<{ scrollHeight: number; scrollTop: number; headId: string | null } | null>(null);
  // 用户往上翻的时候不要把他拽回底部。
  const stickToBottomRef = useRef(true);
  // 点过「新建任务」之后，服务端挑出来的会话必须被忽略一次。
  const freshTaskRef = useRef(false);

  // 扫码进入时 URL 里带 session / cwd。
  useEffect(() => {
    setTarget(readParams());
  }, []);

  /** 拉一次快照；返回「这轮已经不在跑了」供 SSE 侧决定要不要收掉流式尾巴。 */
  const refresh = useCallback(async (): Promise<boolean> => {
    if (!target) return false;
    try {
      const query = new URLSearchParams({ limit: String(limit) });
      if (target.session) query.set("session", target.session);
      if (target.cwd) query.set("cwd", target.cwd);
      const response = await fetch(`/api/mobile/state?${query.toString()}`);
      const data = await response.json() as MobileStateSnapshot & { error?: string };
      if (!response.ok || data.error) {
        setError(data.error ?? `HTTP ${response.status}`);
        return false;
      }
      setError(null);
      setLoadingEarlier(false);
      // 快照是权威状态：它对账事件带来的卡片（没等到的补上、已被别处答过的收掉）。
      setEventPrompt(data.pendingUiRequest ?? null);
      // 新建任务模式下服务端仍会挑出「最近更新的会话」，这里必须忽略它，
      // 否则刚点完「新建任务」就被拉回原来那个会话。这份快照也不参与本地气泡接管。
      if (!target.session && freshTaskRef.current) {
        setSnapshot({
          sessionId: null,
          name: null,
          cwd: data.cwd ?? target.cwd,
          status: "idle",
          updatedAt: null,
          queue: { steering: 0, followUp: 0 },
          messages: [],
        });
        return false;
      }
      // 「新增条目」必须在写回已见集合之前算好：updater 是稍后才跑的，
      // 直接传 seenMessageIdsRef.current 的引用会被刚刚写回的 id 污染。
      const knownIds = new Set(seenMessageIdsRef.current);
      for (const message of data.messages) seenMessageIdsRef.current.add(message.id);
      setPending((previous) => reconcilePendingUserMessages(previous, data.messages, knownIds));
      setSnapshot(data);
      // 跑完先让快照接管，再收流式尾巴：内容不会先消失再出现，新挂起的折叠组
      // 也在同一帧直接是收起状态（而不是先展开、下一帧再折回去）。
      const settled = data.status !== "running";
      if (settled) dispatch({ type: "end" });
      // 只有 URL 没指定会话时，才接管服务端挑中的那一个。
      if (!target.session && data.sessionId) {
        const next = { session: data.sessionId, cwd: target.cwd ?? data.cwd };
        setTarget(next);
        window.history.replaceState(null, "", `/m?session=${encodeURIComponent(data.sessionId)}${next.cwd ? `&cwd=${encodeURIComponent(next.cwd)}` : ""}`);
      }
      return settled;
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setLoadingEarlier(false);
      return false;
    }
  }, [limit, target]);

  useEffect(() => {
    void refresh();
  }, [refresh]);

  // SSE 只跟会话绑定：`limit` / `target` 变化会重建 refresh，但不应该重订事件流。
  const refreshRef = useRef(refresh);
  useEffect(() => {
    refreshRef.current = refresh;
  }, [refresh]);

  // 运行计时：从第一帧流式标记开始算。
  const running = snapshot?.status === "running" || streaming.isStreaming;
  useEffect(() => {
    if (!running) {
      startedAtRef.current = null;
      setElapsed(0);
      return;
    }
    if (startedAtRef.current === null) startedAtRef.current = Date.now() - elapsed * 1000;
    const timer = window.setInterval(() => {
      const startedAt = startedAtRef.current;
      if (startedAt !== null) setElapsed(Math.floor((Date.now() - startedAt) / 1000));
    }, 1000);
    return () => window.clearInterval(timer);
    // elapsed 只用于首帧补算，不参与依赖，避免每秒重建定时器。
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [running]);

  // SSE：流式尾巴 + 终态后补一次快照。切到后台就断开，回到前台先补快照再重连。
  const sessionId = target?.session ?? null;
  useEffect(() => {
    if (!sessionId) return;
    let source: EventSource | null = null;
    let closed = false;

    const connect = () => {
      if (closed || source) return;
      let firstOpen = true;
      source = new EventSource(`/api/agent/${encodeURIComponent(sessionId)}/events`);
      // EventSource 自己重连，但断线期间的事件不会重放：重连成功就补一次快照。
      source.onopen = () => {
        if (firstOpen) {
          firstOpen = false;
          return;
        }
        void refreshRef.current();
      };
      source.onmessage = (event) => {
        let payload: MobileStreamPayload;
        try {
          payload = JSON.parse(event.data) as MobileStreamPayload;
        } catch {
          return;
        }
        lastEventAtRef.current = Date.now();
        switch (classifyAgentEvent(payload)) {
          case "start":
            dispatch({ type: "start" });
            // 这一轮可能是桌面端（或 CLI / 扩展）开始的：先把运行状态同步过来。
            void refreshRef.current();
            // 提问正文这时候还拿不到：pi 在 `message_end` 之后才写会话文件，而 `agent_start`
            // 比它更早。所以第一次有正文流进来时再补一次快照，那时提问一定已经落盘。
            syncOnFirstDeltaRef.current = true;
            return;
          case "snapshot":
            // 流式气泡的起点。少了它 `streamReducer` 会丢弃之后所有 delta，
            // 手机端就变成「要么没内容，要么这轮结束后一下子全出来」。
            dispatch({ type: "snapshot", message: payload.message as never });
            return;
          case "prompt":
            // 扩展在等回答：事件自带请求体，先弹出来；随后的快照会对账校正。
            setEventPrompt(blockingUiRequestFromEvent(payload));
            return;
          case "delta":
            dispatch({ type: "delta", event: payload.assistantMessageEvent as never });
            if (syncOnFirstDeltaRef.current) {
              syncOnFirstDeltaRef.current = false;
              void refreshRef.current();
            }
            return;
          case "settle":
            syncOnFirstDeltaRef.current = false;
            // 尾巴由 refresh 里随快照一起收掉；快照没拿回来时兜底直接收尾，
            // 否则手机上会一直转着那个「正在输出」的状态。
            void refreshRef.current().then((settled) => {
              if (!settled) dispatch({ type: "end" });
            });
            return;
          case "sync":
            // 运行中从另一边又发了一条（排队/插话）：正文只存在于快照里。
            // `extension_ui_resolved` 也走这里：先把已作废的卡片立刻收掉（事件带 id），
            // 再补一次快照兜底，不依赖取数快慢。
            if (payload.type === "extension_ui_resolved") {
              const resolvedId = typeof payload.id === "string" ? payload.id : null;
              setEventPrompt((current) => (resolvedId && current?.id === resolvedId ? null : current));
            }
            void refreshRef.current();
            return;
          default:
            return;
        }
      };
      // onerror 只负责让 EventSource 自己重连，补快照交给 onopen。
      source.onerror = () => {};
    };

    const handleVisibility = () => {
      if (document.visibilityState === "hidden") {
        source?.close();
        source = null;
        return;
      }
      void refreshRef.current();
      connect();
    };

    connect();
    document.addEventListener("visibilitychange", handleVisibility);
    return () => {
      closed = true;
      document.removeEventListener("visibilitychange", handleVisibility);
      source?.close();
      source = null;
    };
  }, [sessionId]);

  // 网络回来时补一次快照：断线期间结束的那一轮不会重放事件。
  useEffect(() => {
    const onOnline = () => {
      void refreshRef.current();
    };
    window.addEventListener("online", onOnline);
    return () => window.removeEventListener("online", onOnline);
  }, []);

  // 看门狗：在跑但事件静默太久（半开连接不会触发 onerror，服务端也没往下发东西），
  // 就主动补一次快照，别让手机一直停在上一次的内容上。页面在后台时不看不打。
  useEffect(() => {
    if (!running) return;
    const timer = window.setInterval(() => {
      if (document.visibilityState === "hidden") return;
      const last = lastEventAtRef.current;
      if (last === null || Date.now() - last < IDLE_STREAM_REFRESH_MS) return;
      // 先记时间：即使这次拉回来的快照是旧的，也不要每个 tick 都打一炮。
      lastEventAtRef.current = Date.now();
      void refreshRef.current();
    }, STREAM_WATCHDOG_TICK_MS);
    return () => window.clearInterval(timer);
  }, [running]);

  const messages = useMemo(() => snapshot?.messages ?? [], [snapshot]);  // 按「用户消息 → 处理详情 → 最终回答」分组，规则见 lib/mobile-timeline.ts。
  // 本地待确认的用户消息接在快照后面，让流式尾巴落进它们开启的那一轮。
  const timeline = useMemo(() => buildMobileTimeline([...messages, ...pending]), [messages, pending]);

  const liveTail = useMemo(() => {
    const message = streaming.streamingMessage;
    if (!message) return null;
    const { text, thinking, tools } = projectAssistantBlocks((message as { content?: unknown }).content);
    if (!text && !thinking && tools.length === 0) return null;
    return { text, thinking, tools };
  }, [streaming.streamingMessage]);

  const scrollToBottom = useCallback((behavior: ScrollBehavior = "auto") => {
    const container = scrollRef.current;
    if (!container) return;
    container.scrollTo({ top: container.scrollHeight, behavior });
    stickToBottomRef.current = true;
    setShowJump(false);
  }, []);

  // 只在「贴底 / 离开贴底」这一位上翻转状态，滚动过程本身不重渲染。
  const handleScroll = useCallback(() => {
    const container = scrollRef.current;
    if (!container) return;
    const distanceFromBottom = container.scrollHeight - container.scrollTop - container.clientHeight;
    const stuck = distanceFromBottom < STICK_THRESHOLD_PX;
    stickToBottomRef.current = stuck;
    // 锚点要的是「用户现在在看哪」，滚动过程中持续更新，否则可能拿着一份很旧的位置去算。
    if (lastLayoutRef.current) lastLayoutRef.current.scrollTop = container.scrollTop;
    setShowJump((previous) => (previous === !stuck ? previous : !stuck));
  }, []);

  useEffect(() => {
    const container = scrollRef.current;
    if (!container) return;
    const headId = messages[0]?.id ?? null;
    const previous = lastLayoutRef.current;

    if (previous && previous.headId !== headId && !stickToBottomRef.current) {
      // 头部变了（往回加载，或被新消息挤掉最旧的一条）：保持正在看的那条不动。
      container.scrollTop = container.scrollHeight - previous.scrollHeight + previous.scrollTop;
    } else if (stickToBottomRef.current) {
      container.scrollTop = container.scrollHeight;
    }

    lastLayoutRef.current = { scrollHeight: container.scrollHeight, scrollTop: container.scrollTop, headId };
  }, [messages, pending.length, liveTail?.text, liveTail?.tools.length]);

  // 输入框是自增长高度的（`style.height` 直接写上去的），所以清空时得手动收回一行，
  // 否则发完之后它会一直停在之前那次最高的高度上，把转录区顶掉一截。
  useEffect(() => {
    if (input !== "") return;
    const element = composerRef.current;
    if (element) element.style.height = "auto";
  }, [input]);

  // 回答扩展请求（`ask_user` 等）：同一个会话，同一个答案；桌面端会收到
  // `extension_ui_resolved` 把自己的弹窗收掉，这里则由下一次快照把卡片移除。
  const answerPrompt = useCallback(async (request: MobileUiRequest, response: { value: string } | { confirmed: boolean } | { cancelled: true }) => {
    const sid = target?.session ?? null;
    if (!sid || answeringPrompt) return;
    setAnsweringPrompt(true);
    try {
      const result = await fetch(`/api/agent/${encodeURIComponent(sid)}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ type: "extension_ui_response", id: request.id, ...response }),
      });
      const data = await result.json() as { error?: string };
      if (!result.ok || data.error) throw new Error(data.error ?? `HTTP ${result.status}`);
      await refreshRef.current();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setAnsweringPrompt(false);
    }
  }, [answeringPrompt, target?.session]);

  // 往回翻一批：窗口拉大后由 refresh 重新取快照，滚动位置由上面的锚点接管。
  const loadEarlier = useCallback(() => {
    setLoadingEarlier(true);
    setLimit((value) => Math.min(value + MOBILE_LIMIT_STEP, MOBILE_MAX_LIMIT));
  }, []);

  const send = useCallback(async () => {
    const text = input.trim();
    if (!text || busy) return;
    setBusy(true);
    setError(null);
    try {
      if (sessionId) {
        const response = await fetch(`/api/agent/${encodeURIComponent(sessionId)}`, {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          // 运行中不带 streamingBehavior 会被 SDK 直接拒绝（“Agent is already
          // processing…”），手机上就变成发不出去。带 steer 后会话有空时
          // SDK 仍会开新一轮，所以这一条路径不需要分支。
          body: JSON.stringify({ type: "prompt", message: text, ...(running ? { streamingBehavior: "steer" } : {}) }),
        });
        const data = await response.json() as { error?: string };
        if (!response.ok || data.error) throw new Error(data.error ?? `HTTP ${response.status}`);
      } else {
        const targetCwd = target?.cwd ?? snapshot?.cwd ?? null;
        if (!targetCwd) throw new Error(t("mobile.needProject"));
        const response = await fetch("/api/agent/new", {
          method: "POST",
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify({ cwd: targetCwd, type: "prompt", message: text }),
        });
        const data = await response.json() as { sessionId?: string; error?: string };
        if (!response.ok || data.error) throw new Error(data.error ?? `HTTP ${response.status}`);
        if (data.sessionId) {
          freshTaskRef.current = false;
          setTarget({ session: data.sessionId, cwd: targetCwd });
          window.history.replaceState(null, "", `/m?session=${encodeURIComponent(data.sessionId)}&cwd=${encodeURIComponent(targetCwd)}`);
        }
      }
      setInput("");
      // 乐观插入：服务端要等这轮结束才重新拉快照，先让用户看到自己刚发的那句话。
      const sentAt = new Date();
      setPending((previous) => [
        ...previous,
        { id: `pending-${pendingIdRef.current++}`, role: "user", text, at: formatClock(sentAt) },
      ]);
      dispatch({ type: "start" });
      startedAtRef.current = Date.now();
      scrollToBottom();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setBusy(false);
    }
  }, [busy, input, running, scrollToBottom, sessionId, snapshot?.cwd, t, target?.cwd]);

  const stop = useCallback(async () => {
    if (!sessionId) return;
    try {
      await fetch(`/api/agent/${encodeURIComponent(sessionId)}`, {
        method: "POST",
        headers: { "Content-Type": "application/json" },
        body: JSON.stringify({ type: "abort" }),
      });
      dispatch({ type: "end" });
      await refresh();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    }
  }, [refresh, sessionId]);

  // 新建任务：脱离当前会话，下一次发送会在这个项目里开一个新会话。
  const startNewTask = useCallback(() => {
    const targetCwd = snapshot?.cwd ?? target?.cwd ?? null;
    freshTaskRef.current = true;
    setTarget({ session: null, cwd: targetCwd });
    setSnapshot(null);
    setInput("");
    setPending([]);
    setLimit(MOBILE_DEFAULT_LIMIT);
    dispatch({ type: "end" });
    window.history.replaceState(null, "", targetCwd ? `/m?cwd=${encodeURIComponent(targetCwd)}` : "/m");
  }, [snapshot?.cwd, target?.cwd]);

  const statusLabel = running ? t("mobile.statusRunning") : t("mobile.statusIdle");
  const queued = (snapshot?.queue.steering ?? 0) + (snapshot?.queue.followUp ?? 0);
  const cwdLabel = snapshot?.cwd ?? target?.cwd ?? "";
  const canSend = Boolean(input.trim()) && !busy;
  const earlierCount = snapshot?.earlierCount ?? 0;
  const cappedByLimit = limit >= MOBILE_MAX_LIMIT;
  const pendingUiRequest = eventPrompt ?? snapshot?.pendingUiRequest ?? null;

  const paramsReady = target !== null;
  const composing = paramsReady && !sessionId;
  const transcriptEmpty = messages.length === 0 && pending.length === 0 && !liveTail;
  const loading = !paramsReady || (!composing && snapshot === null && !error);
  const showEmptyState = !loading && transcriptEmpty && !running;
  const sessionTitle = sessionId ? snapshot?.name ?? sessionId.slice(0, 8) : "";

  return (
    <div
      style={{
        height: "var(--app-viewport-height, 100dvh)",
        display: "flex",
        flexDirection: "column",
        background: "var(--bg)",
        color: "var(--text)",
        paddingTop: "env(safe-area-inset-top)",
        paddingBottom: "env(safe-area-inset-bottom)",
        paddingLeft: "env(safe-area-inset-left)",
        paddingRight: "env(safe-area-inset-right)",
      }}
    >
      <header style={{ flexShrink: 0, display: "flex", flexDirection: "column", gap: 7, padding: "10px 14px", borderBottom: "1px solid var(--border)" }}>
        <div style={{ display: "flex", alignItems: "center", gap: 10 }}>
          <div style={{ display: "flex", alignItems: "center", gap: 7, flex: 1, minWidth: 0 }}>
            <span
              aria-hidden="true"
              className={running ? "mobile-status-dot is-running" : "mobile-status-dot"}
              style={{ width: 7, height: 7, borderRadius: "50%", flexShrink: 0, background: running ? "var(--live)" : "var(--text-muted)" }}
            />
            <span role="status" style={{ fontSize: 13, fontWeight: 600, whiteSpace: "nowrap", color: running ? "var(--live)" : "var(--text-muted)" }}>
              {statusLabel}
            </span>
            {running && (
              <span aria-hidden="true" style={{ fontFamily: "var(--font-mono)", fontSize: 12, fontVariantNumeric: "tabular-nums", color: "var(--text-muted)" }}>
                {formatElapsed(elapsed)}
              </span>
            )}
            {queued > 0 && (
              <span style={{ flexShrink: 0, padding: "1px 6px", borderRadius: MOBILE_RADIUS.chip, border: "1px solid var(--border)", color: "var(--text-muted)", fontSize: 11 }}>
                {t("mobile.queued", { count: queued })}
              </span>
            )}
          </div>
          <button
            type="button"
            className="mobile-tap"
            onClick={() => void stop()}
            disabled={!running}
            style={{
              flexShrink: 0, minHeight: 38, padding: "0 15px", borderRadius: MOBILE_RADIUS.round,
              border: "1px solid var(--border)", background: "none",
              color: running ? "var(--danger)" : "var(--text-dim)",
              cursor: running ? "pointer" : "default", fontSize: 12, fontWeight: 600,
            }}
          >
            {t("mobile.stop")}
          </button>        </div>
        {(sessionTitle || cwdLabel) && (
          <div style={{ display: "flex", alignItems: "baseline", gap: 8, minWidth: 0 }}>
            {sessionTitle && (
              <span style={{ flex: "0 1 auto", minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", fontSize: 12, color: "var(--text)" }}>
                {sessionTitle}
              </span>
            )}
            {cwdLabel && (
              <span style={{ flex: "1 1 auto", minWidth: 0, overflow: "hidden", textOverflow: "ellipsis", whiteSpace: "nowrap", direction: "rtl", textAlign: "left", fontFamily: "var(--font-mono)", fontSize: 11, color: "var(--text-muted)" }}>
                {cwdLabel}
              </span>
            )}
          </div>
        )}
      </header>

      <div style={{ position: "relative", flex: 1, minHeight: 0, display: "flex" }}>
        <div
          ref={scrollRef}
          onScroll={handleScroll}
          style={{ flex: 1, minHeight: 0, overflowY: "auto", overflowX: "hidden", overscrollBehavior: "contain", padding: "14px 14px 18px", display: "flex", flexDirection: "column", gap: 16 }}
        >
          {earlierCount > 0 && (
            <div style={{ display: "flex", justifyContent: "center" }}>
              {cappedByLimit ? (
                <span style={{ fontSize: 11, color: "var(--text-muted)", textAlign: "center" }}>
                  {t("mobile.historyCapped")}
                </span>
              ) : (
                <button
                  type="button"
                  className="mobile-tap"
                  onClick={loadEarlier}
                  disabled={loadingEarlier}
                  style={{
                    minHeight: 36, padding: "0 14px", borderRadius: MOBILE_RADIUS.round,
                    border: "1px solid var(--border)", background: "var(--bg-panel)",
                    color: loadingEarlier ? "var(--text-dim)" : "var(--text-muted)",
                    cursor: loadingEarlier ? "default" : "pointer", fontSize: 12,
                  }}
                >
                  {loadingEarlier ? t("mobile.loading") : t("mobile.loadEarlier", { count: earlierCount })}
                </button>
              )}
            </div>
          )}

          {timeline.map((item, index) => {
            const inFlight = running && index === timeline.length - 1;
            return (
              <div
                key={item.key}
                role="group"
                aria-label={item.kind === "user" ? t("mobile.you") : t("mobile.assistant")}
                style={item.kind === "user"
                  ? { display: "flex", justifyContent: "flex-end" }
                  : { display: "flex", flexDirection: "column", gap: 8, minWidth: 0 }}
              >
                {item.kind === "user" ? (
                  <div
                    style={{
                      maxWidth: "90%", display: "flex", flexDirection: "column", gap: 2,
                      padding: "8px 11px 6px", borderRadius: MOBILE_RADIUS.bubble,
                      background: "var(--user-bg)", border: "1px solid color-mix(in srgb, var(--accent) 14%, transparent)",
                    }}
                  >
                    <p style={{ margin: 0, fontSize: 14, lineHeight: 1.6, whiteSpace: "pre-wrap", overflowWrap: "anywhere", color: "var(--text)" }}>
                      {item.message.text}
                    </p>
                    {item.message.at && (
                      <span style={{ alignSelf: "flex-end", fontFamily: "var(--font-mono)", fontSize: 10, color: "var(--text-muted)" }}>
                        {item.message.at}
                      </span>
                    )}
                  </div>
                ) : (
                  <>
                    {(item.messageCount > 0 || item.toolCallCount > 0) && (
                      <MobileProcessDetails
                        messageCount={item.messageCount}
                        toolCallCount={item.toolCallCount}
                        running={inFlight}
                      >
                        {item.process.map((message) => (
                          <div key={message.id} style={{ display: "flex", flexDirection: "column", gap: 6, minWidth: 0 }}>
                            {message.thinking && <MobileThinking text={message.thinking} />}
                            {message.tools && message.tools.length > 0 && <MobileToolList calls={message.tools} />}
                            {message.text && <MobileMarkdown text={message.text} cwd={cwdLabel || null} />}
                          </div>
                        ))}
                        {item.answer?.tools && item.answer.tools.length > 0 && <MobileToolList calls={item.answer.tools} />}
                      </MobileProcessDetails>
                    )}
                    {item.answer?.text && <MobileMarkdown text={item.answer.text} cwd={cwdLabel || null} />}
                  </>
                )}
              </div>
            );
          })}

          {liveTail && (
            <div role="group" aria-label={t("mobile.assistant")} style={{ display: "flex", flexDirection: "column", gap: 8, minWidth: 0 }}>
              {(liveTail.tools.length > 0 || Boolean(liveTail.thinking)) && (
                <MobileProcessDetails
                  messageCount={1}
                  toolCallCount={liveTail.tools.length}
                  running={streaming.isStreaming}
                >
                  {liveTail.thinking && <MobileThinking text={liveTail.thinking} />}
                  {liveTail.tools.length > 0 && <MobileToolList calls={liveTail.tools} />}
                </MobileProcessDetails>
              )}
              {liveTail.text && <MobileMarkdown text={liveTail.text} cwd={cwdLabel || null} streaming />}
            </div>
          )}

          {running && !liveTail && !loading && <TypingPulse />}

          {loading && <TranscriptSkeleton label={t("mobile.loading")} />}

          {showEmptyState && (
            <div style={{ margin: "auto", display: "flex", flexDirection: "column", gap: 6, maxWidth: 260, textAlign: "center" }}>
              <p style={{ margin: 0, fontSize: 13, lineHeight: 1.65, color: "var(--text-muted)" }}>
                {composing ? t("mobile.newTaskHint") : t("mobile.emptySession")}
              </p>
              {composing && cwdLabel && (
                <span style={{ fontFamily: "var(--font-mono)", fontSize: 11, overflowWrap: "anywhere", color: "var(--text-muted)" }}>
                  {cwdLabel}
                </span>
              )}
            </div>
          )}
        </div>

        {showJump && !loading && (
          <button
            type="button"
            className="mobile-tap"
            onClick={() => scrollToBottom(window.matchMedia("(prefers-reduced-motion: reduce)").matches ? "auto" : "smooth")}
            aria-label={t("mobile.jumpToLatest")}
            style={{
              position: "absolute", left: 0, right: 0, bottom: 12, margin: "0 auto",
              width: 38, height: 38, display: "flex", alignItems: "center", justifyContent: "center",
              borderRadius: MOBILE_RADIUS.round, border: "1px solid var(--border)", background: "var(--bg-panel)",
              color: "var(--text-muted)", cursor: "pointer",
              boxShadow: "0 6px 18px color-mix(in srgb, var(--text) 16%, transparent)",
            }}
          >
            <ArrowDown size={16} aria-hidden="true" />
          </button>
        )}
      </div>

      {error && (
        <div
          role="alert"
          style={{
            flexShrink: 0, margin: "0 14px 10px", padding: "8px 10px", borderRadius: MOBILE_RADIUS.chip,
            background: "color-mix(in srgb, var(--danger) 8%, var(--bg))",
            border: "1px solid color-mix(in srgb, var(--danger) 32%, var(--border))",
            color: "var(--danger)", fontSize: 12, lineHeight: 1.5, overflowWrap: "anywhere",
          }}
        >
          {error}
        </div>
      )}

      {pendingUiRequest && (
        <MobilePromptCard request={pendingUiRequest} onRespond={answerPrompt} busy={answeringPrompt} />
      )}

      <div style={{ flexShrink: 0, padding: "10px 14px 12px", borderTop: "1px solid var(--border)" }}>
        <div
          style={{
            display: "flex", alignItems: "flex-end", gap: 6, padding: 6,
            borderRadius: MOBILE_RADIUS.field,
            border: `1px solid ${composerFocused ? "color-mix(in srgb, var(--accent) 55%, var(--border))" : "var(--border)"}`,
            background: "var(--bg)",
            boxShadow: composerFocused ? "0 0 0 3px color-mix(in srgb, var(--accent) 14%, transparent)" : "none",
            transition: "border-color 150ms, box-shadow 150ms",
          }}
        >
          <button
            type="button"
            className="mobile-tap"
            onClick={startNewTask}
            disabled={composing}
            aria-label={t("mobile.newTask")}
            title={t("mobile.newTask")}
            style={{
              flexShrink: 0, width: 40, height: 40, display: "flex", alignItems: "center", justifyContent: "center",
              borderRadius: MOBILE_RADIUS.round, border: "1px solid var(--border)",
              background: composing ? "none" : "var(--bg-panel)",
              color: composing ? "var(--text-dim)" : "var(--text-muted)",
              cursor: composing ? "default" : "pointer",
            }}
          >
            <Plus size={18} aria-hidden="true" />
          </button>
          <textarea
            ref={composerRef}
            className="mobile-composer-input"
            value={input}
            onChange={(event) => {
              setInput(event.target.value);
              const element = event.currentTarget;
              element.style.height = "auto";
              element.style.height = `${Math.min(element.scrollHeight, COMPOSER_MAX_HEIGHT)}px`;
            }}
            onFocus={() => setComposerFocused(true)}
            onBlur={() => setComposerFocused(false)}
            onKeyDown={(event) => {
              if (event.key === "Enter" && !event.shiftKey) {
                event.preventDefault();
                void send();
              }
            }}
            rows={1}
            enterKeyHint="send"
            autoComplete="off"
            spellCheck={false}
            placeholder={running ? t("mobile.placeholderSteer") : t("mobile.placeholder")}
            aria-label={t("mobile.placeholder")}
            style={{
              flex: 1, minWidth: 0, resize: "none", maxHeight: COMPOSER_MAX_HEIGHT,
              padding: "8px 2px", border: "none", background: "transparent",
              color: "var(--text)", outline: "none", fontFamily: "inherit",
              fontSize: 16, lineHeight: 1.5,
            }}
          />
          <button
            type="button"
            className="mobile-tap"
            onClick={() => void send()}
            disabled={!canSend}
            aria-label={t("mobile.send")}
            style={{
              flexShrink: 0, width: 40, height: 40, display: "flex", alignItems: "center", justifyContent: "center",
              borderRadius: MOBILE_RADIUS.round, border: "none",
              background: canSend ? "var(--accent)" : "var(--bg-panel)",
              // 圆底的图标色跟随页面底色：浅色主题下是白，深色主题下是近黑，两侧都够对比度。
              color: canSend ? "var(--bg)" : "var(--text-dim)",
              cursor: canSend ? "pointer" : "default",
            }}
          >
            <ArrowUp size={18} aria-hidden="true" />
          </button>
        </div>
      </div>
    </div>
  );
}
