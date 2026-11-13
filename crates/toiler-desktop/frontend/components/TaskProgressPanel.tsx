"use client";

import {
  Check,
  ChevronDown,
  Circle,
  CircleDot,
  Minus,
  OctagonAlert,
  X,
} from "lucide-react";
import { useCallback, useEffect, useId, useLayoutEffect, useMemo, useRef, useState, type ReactNode } from "react";
import { useI18n } from "@/hooks/useI18n";
import {
  summarizeTaskProgress,
  type TaskProgressEntry,
  type TaskStatus,
} from "@/lib/task-progress-view";
import styles from "./TaskProgressPanel.module.css";

/**
 * 任务进度面板（`task_progress` 工具的实体）。
 *
 * 位置：贴在输入框上方、与输入框同宽（由 `ChatInput` 的 `aboveComposer` 渲染进
 * 那条 820px 的内容列）。任务进度是「这一轮在做什么」的进程状态：
 * 放在聊天流里会随滚动跑掉，放到右侧栏又与输入动作脱节 —— 用户读它的时机
 * 恰恰是「准备下一条输入」之前。
 *
 * 层级：与输入框同一种材质（`--bg` 面 + 1px 边框 + `--radius-lg`），但**不给阴影**。
 * 阴影在本项目里只属于两类「浮在内容面上」的东西（输入框外壳、弹层），
 * 再叠一层浮起的面会让层级读不清。
 */

const COLLAPSE_STORAGE_KEY = "toiler.task-progress-collapsed";
/** 全部打完后先让用户看见结果，再收掉（与后端一致：它这时已经不推 widget 了）。 */
const FINISHED_LINGER_MS = 1800;

const STATUS_ICON: Record<TaskStatus, ReactNode> = {
  pending: <Circle size={13} strokeWidth={2} aria-hidden="true" />,
  in_progress: <CircleDot size={13} strokeWidth={2.4} aria-hidden="true" />,
  completed: <Check size={13} strokeWidth={2.6} aria-hidden="true" />,
  failed: <X size={13} strokeWidth={2.6} aria-hidden="true" />,
  blocked: <OctagonAlert size={13} strokeWidth={2.2} aria-hidden="true" />,
  skipped: <Minus size={13} strokeWidth={2.6} aria-hidden="true" />,
};

/** 只有「还在解释自己」的状态才值得占一行小字。 */
const DETAIL_STATUSES: ReadonlySet<TaskStatus> = new Set<TaskStatus>(["in_progress", "failed", "blocked"]);

function readCollapsed(): boolean {
  try {
    return window.localStorage.getItem(COLLAPSE_STORAGE_KEY) === "1";
  } catch {
    return false;
  }
}

function writeCollapsed(collapsed: boolean): void {
  try {
    window.localStorage.setItem(COLLAPSE_STORAGE_KEY, collapsed ? "1" : "0");
  } catch {
    // 隐私模式下写不进去就算了：折叠状态只是偏好，不该影响渲染。
  }
}

export function TaskProgressPanel({ tasks }: { tasks: TaskProgressEntry[] }) {
  const { t } = useI18n();
  const listId = useId();
  const [collapsed, setCollapsed] = useState(readCollapsed);
  const listRef = useRef<HTMLOListElement>(null);
  const activeRef = useRef<HTMLLIElement>(null);
  const sawUnfinishedRef = useRef(false);
  const [finishedLinger, setFinishedLinger] = useState(false);

  const summary = useMemo(() => summarizeTaskProgress(tasks), [tasks]);

  // 打开一个「任务早就全做完了」的历史会话时不该闪一下：只有亲眼见过未完成状态，
  // 才有资格播放收尾的那 1.8 秒。
  useEffect(() => {
    if (summary.total === 0) {
      sawUnfinishedRef.current = false;
      setFinishedLinger(false);
      return;
    }
    if (!summary.finished) {
      sawUnfinishedRef.current = true;
      setFinishedLinger(false);
      return;
    }
    if (!sawUnfinishedRef.current) return;
    setFinishedLinger(true);
    const timer = window.setTimeout(() => setFinishedLinger(false), FINISHED_LINGER_MS);
    return () => window.clearTimeout(timer);
  }, [summary.finished, summary.total]);

  const toggle = useCallback(() => {
    setCollapsed((previous) => {
      writeCollapsed(!previous);
      return !previous;
    });
  }, []);

  // 任务多到要滚动时，先把当前那条摆进视口 —— 面板存在的意义就是「现在做到哪」。
  useLayoutEffect(() => {
    if (collapsed) return;
    const list = listRef.current;
    const item = activeRef.current;
    if (!list || !item) return;
    const itemTop = item.offsetTop;
    const itemBottom = itemTop + item.offsetHeight;
    if (itemTop >= list.scrollTop && itemBottom <= list.scrollTop + list.clientHeight) return;
    list.scrollTop = Math.max(0, itemTop - (list.clientHeight - item.offsetHeight) / 2);
  }, [collapsed, summary.active?.id]);

  // 全部收尾后后端会把 widget 收掉；历史会话按同一规则收掉，画面保持一致。
  if (summary.total === 0 || (summary.finished && !finishedLinger)) return null;

  const percent = Math.round(summary.ratio * 100);
  const heading = summary.finished
    ? t("chat.taskProgressDone")
    : summary.active
      ? t("chat.taskProgressActive", { title: summary.active.title })
      : t("chat.taskProgressRemaining", { count: summary.total - summary.completed });

  return (
    <section
      className={styles.panel}
      aria-label={t("chat.taskProgress")}
      data-collapsed={collapsed}
      data-finished={summary.finished}
    >
      <button
        type="button"
        className={styles.head}
        onClick={toggle}
        aria-expanded={!collapsed}
        aria-controls={listId}
        title={collapsed ? t("chat.taskProgressExpand") : t("chat.taskProgressCollapse")}
      >
        <span className={styles.count} role="status" aria-live="polite">
          {summary.completed}/{summary.total}
        </span>
        <span className={styles.current}>{heading}</span>
        <ChevronDown size={14} strokeWidth={2.2} className={styles.chevron} aria-hidden="true" />
      </button>

      <div
        className={styles.meter}
        role="progressbar"
        aria-valuemin={0}
        aria-valuemax={summary.total}
        aria-valuenow={summary.completed}
        aria-valuetext={`${summary.completed}/${summary.total}`}
      >
        <span className={styles.meterFill} style={{ transform: `scaleX(${percent / 100})` }} />
      </div>

      <ol className={styles.list} id={listId} ref={listRef} hidden={collapsed}>
        {tasks.map((task) => (
          <li
            key={task.id}
            ref={task.status === "in_progress" ? activeRef : undefined}
            className={styles.item}
            data-status={task.status}
          >
            <span className={styles.mark}>{STATUS_ICON[task.status]}</span>
            <span className={styles.title} title={task.title}>{task.title}</span>
            {task.detail && DETAIL_STATUSES.has(task.status) && (
              <span className={styles.detail} title={task.detail}>{task.detail}</span>
            )}
            <span className={styles.spacer} aria-hidden="true" />
            <span className={styles.srOnly}>{t(`chat.taskStatus.${task.status}`)}</span>
          </li>
        ))}
      </ol>
    </section>
  );
}
