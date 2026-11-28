import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const read = (path) => readFile(new URL(`../${path}`, import.meta.url), "utf8");
const panel = await read("components/GitPanel.tsx");
const history = await read("components/GitHistoryPanel.tsx");
const globals = await read("app/globals.css");
const historyCss = await read("components/GitHistoryPanel.module.css");

/**
 * 打开 Git 工作区时,数据到达前先把分区骨架立起来。
 *
 * 原来两处都只有一行「正在加载」。`/api/git` 与 `/api/git/refs` 一返回,一行文字
 * 就撑成七八屏控件 / 四栏布局,滚动位置和鼠标落点全跳。骨架按最终尺寸占位,
 * 数据到达只换内容 —— 这几个断言守的就是「别再退回单行提示」。
 */
test("Git 操作区加载时显示分区骨架,不再只渲染一行提示", () => {
  assert.match(panel, /\{!summary && !error && <GitPanelSkeleton \/>\}/);
  assert.doesNotMatch(panel, /!summary && !error && <div className="git-hint">/);
  const skeleton = panel.slice(panel.indexOf("function GitPanelSkeleton"), panel.indexOf("function SectionTitle"));
  assert.ok(skeleton.length > 0, "缺少 GitPanelSkeleton");
  // 骨架要覆盖真实分区的顺序:远程 → 变更 → 暂存 → 提交。
  assert.match(skeleton, /git-skeleton-field/);
  assert.equal([...skeleton.matchAll(/className="git-file-list"/g)].length, 2);
});

test("骨架对读屏仍保留加载状态", () => {
  const skeleton = panel.slice(panel.indexOf("function GitPanelSkeleton"), panel.indexOf("function SectionTitle"));
  assert.match(skeleton, /<span className="sr-only" role="status">\{t\("git\.loadingRepository"\)\}<\/span>/);
  // 骨架本身是装饰,不能进无障碍树,也不该被读成内容。
  assert.match(skeleton, /className="git-skeleton" aria-hidden="true"/);
});

test("提交历史加载时三栏骨架就位,出错才落到错误条", () => {
  assert.match(history, /!data && !resource\.error && <>/);
  assert.match(history, /<span className="sr-only" role="status">\{t\("gitHistory\.loading"\)\}<\/span>/);
  // 分支栏、分隔条、提交列表、提交详情四格都要在,宽度才是换算完的最终布局。
  const block = history.slice(history.indexOf("!data && !resource.error"), history.indexOf("!data && resource.error"));
  assert.equal([...block.matchAll(/styles\.column/g)].length, 3);
  assert.match(block, /styles\.refsDivider/);
  assert.match(history, /!data && resource\.error && <div className=\{styles\.loadError\}>/);
  assert.match(history, /\{data && <>/);
});

test("加载骨架尊重减少动效偏好", () => {
  const rule = globals.match(/\n\.git-skeleton \{([^}]+)\}/)?.[1];
  assert.ok(rule, "缺少 .git-skeleton 规则");
  assert.match(rule, /animation:\s*pulse\s/);
  const reduced = globals.slice(globals.indexOf("@media (prefers-reduced-motion: reduce)", globals.indexOf(".git-skeleton-field")));
  assert.match(reduced.slice(0, 200), /\.git-skeleton \{\s*animation:\s*none;/);

  assert.match(historyCss, /\.skeleton \{[^}]*animation:\s*git-history-skeleton-pulse/);
  assert.match(historyCss, /@media \(prefers-reduced-motion: reduce\) \{\s*\.skeleton \{ animation: none;/);
});

test("骨架行高与真实行一致,数据到达不跳高度", () => {
  const row = historyCss.match(/\n\.skeletonRow \{([^}]+)\}/)?.[1];
  assert.ok(row);
  // 提交行 button.commitGrid 与引用行 .refRow 的高度是 28–30px。
  assert.match(row, /min-height:\s*28px;/);
  const bar = globals.match(/\n\.git-skeleton-bar,\n/)?.[0];
  assert.ok(bar, "Git 操作区骨架条要与其他骨架块共用基础样式");
  // 骨架多包了一层容器,不吃 `.git-panel-body > .git-section:first-child` 的归零,
  // 否则首块顶部多 30px、数据到达时整块往上跳。
  assert.match(globals, /\.git-skeleton > \.git-section:first-child \{\s*margin-top: 0;\s*padding-top: 0;/);
});