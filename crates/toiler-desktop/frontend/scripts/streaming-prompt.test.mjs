import assert from "node:assert/strict";
import { readFile } from "node:fs/promises";
import test from "node:test";

const source = await readFile(new URL("../hooks/useAgentSession.ts", import.meta.url), "utf8");

function callback(name) {
  const match = source.match(new RegExp(`const ${name} = useCallback\\([\\s\\S]*?\\n  \\}, \\[.*\\]\\);`));
  assert.ok(match, `缺少 ${name}`);
  return match[0];
}

for (const [label, handler, behavior] of [
  ["排队", "handleFollowUp", "followUp"],
  ["引导", "handleSteer", "steer"],
]) {
  test(`上一轮已结束时确认${label}：先订阅事件，开新一轮时读取已写入的用户消息`, () => {
    assert.match(callback(handler), new RegExp(`await sendStreamingPrompt\\(message, "${behavior}", images\\)`));
    const send = callback("sendStreamingPrompt");
    assert.match(send, /await ensureEventsConnected\(sid\);[\s\S]*?await sendAgentCommand[\s\S]*?streamingBehavior: behavior/);
    assert.doesNotMatch(send, /setMessages\(/, "仍在排队的消息不能提前加进对话记录");
    const events = callback("handleAgentEvent");
    assert.match(events, /case "agent_start":[\s\S]*?if \(sid && !optimisticUserMessageKeyRef\.current\) void loadSession\(sid\);[\s\S]*?case "agent_end":/);
    assert.match(events, /case "agent_end":[\s\S]*?optimisticUserMessageKeyRef\.current = null;[\s\S]*?case "agent_settled":/);
  });
}

test("旧快照不能覆盖新一轮或刚收到的实时消息", () => {
  const load = callback("loadSession");
  assert.match(load, /const transcriptVersion = \+\+transcriptVersionRef\.current;/);
  assert.match(load, /transcriptVersionRef\.current === transcriptVersion/);
  assert.match(load, /await res\.json\(\)[\s\S]*?if \(!isCurrentTranscript\(\)\) return null;[\s\S]*?setMessages\(persistedMessages\)/);
  const events = callback("handleAgentEvent");
  assert.match(events, /case "agent_start":[\s\S]*?transcriptVersionRef\.current \+= 1/);
  assert.match(events, /case "message_end":[\s\S]*?if \(completed\) transcriptVersionRef\.current \+= 1/);
});

test("新一轮开始后，旧一轮的空闲检查不能结束新一轮", () => {
  assert.match(callback("handleAgentEvent"), /case "agent_start":[\s\S]*?agentStartGenerationRef\.current \+= 1/);
  for (const name of ["reconcileAgentState", "finishPromptWithoutStream"]) {
    assert.match(callback(name), /const agentStartGeneration = agentStartGenerationRef\.current;[\s\S]*?await [\s\S]*?agentStartGenerationRef\.current !== agentStartGeneration\) return;/);
  }
});
