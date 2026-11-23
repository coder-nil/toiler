import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("../hooks/useAgentSession.ts", import.meta.url), "utf8");

function callback(name) {
  const match = source.match(new RegExp(`const ${name} = useCallback\\([\\s\\S]*?\\n  \\}, \\[.*\\]\\);`));
  assert.ok(match, `缺少 ${name}`);
  return match[0];
}

test("系统提示词和斜杠建议不为草稿创建会话", () => {
  const prompt = callback("loadSystemPrompt");
  assert.match(prompt, /fetch\("\/api\/agent\/draft\/system-prompt"/);
  assert.doesNotMatch(prompt, /ensureNewSession/);
  assert.doesNotMatch(callback("loadSlashCommands"), /ensureNewSession/);
});

test("首轮发送先等后端接受，侧栏收到消息落盘通知后刷新", () => {
  assert.match(callback("handleSend"), /await sendAgentCommand\(sid, \{\s+type: "prompt",[\s\S]*?\}\);\s*(?:\/\/[^\n]*\n\s*)?promoteNewSession\(1, message\)/);
  assert.match(callback("handleAgentEvent"), /case "session_published":\s+onSessionListRefresh\?\.\(\)/);
});
