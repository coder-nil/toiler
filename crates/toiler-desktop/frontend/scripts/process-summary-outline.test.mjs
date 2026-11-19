import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

/**
 * 对话里「思考」与「工具调用」两块摘要的外观约定：
 *
 * - 只保留一道 `border-left: 2px solid`，不再套整圈边框；
 * - 颜色必须是不透明的主题 token（`--text-muted` / `--live` / `--danger`），
 *   不用 `rgba(...)` 这类半透明色 —— 半透明色在浅 / 深两套主题里会各自糊成
 *   一档灰，2px 细线在深色底上几乎看不见。
 */

const source = await readFile(new URL("../components/MessageView.tsx", import.meta.url), "utf8");

function functionBody(name, nextName) {
  const start = source.indexOf(`function ${name}(`);
  assert.notEqual(start, -1, `找不到 ${name}`);
  const end = source.indexOf(`function ${nextName}(`, start);
  assert.notEqual(end, -1, `${name} 之后找不到 ${nextName}`);
  return source.slice(start, end);
}

test("思考摘要只用一道不带透明的左侧实线", () => {
  const body = functionBody("ThinkingBlock", "ToolCallBlock");
  assert.ok(
    body.includes('borderLeft: "2px solid var(--text-muted)"'),
    "思考摘要的左边线应该是 2px solid var(--text-muted)",
  );
  assert.doesNotMatch(body, /border:\s*"1px solid/, "思考摘要不该再套整圈边框");
  assert.doesNotMatch(body, /borderLeft:[^,]*rgba\(/, "左边线颜色不能用半透明色");
});

test("工具调用摘要的左边线随成功 / 失败切换，且不带透明", () => {
  const body = functionBody("ToolCallBlock", "PairedDiffResult");
  assert.ok(
    body.includes('borderLeft: isError ? "2px solid var(--danger)" : "2px solid var(--live)"'),
    "工具调用摘要的左边线应该按 isError 在 --danger / --live 之间切换",
  );
  assert.doesNotMatch(body, /border:\s*"1px solid/, "工具调用摘要不该再套整圈边框");
  assert.doesNotMatch(body, /borderLeft:[^,]*rgba\(/, "左边线颜色不能用半透明色");
});

test("工具调用摘要的底色与边线同色系，透明度为 0.15", () => {
  const body = functionBody("ToolCallBlock", "PairedDiffResult");
  assert.ok(
    body.includes('background: isError ? "rgba(248,113,113,0.15)" : "rgba(34,197,94,0.15)"'),
    "摘要底色应该是 0.15 透明度的成功绿 / 失败红",
  );
});
