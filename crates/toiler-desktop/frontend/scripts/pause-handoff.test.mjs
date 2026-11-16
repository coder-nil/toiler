import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const { PauseHandoff } = await createJiti(import.meta.url).import("../lib/pause-handoff.ts");

function deferred() {
  let resolve, reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  return { promise, resolve, reject };
}

test("提交等待暂停完成后才放行", async () => {
  const handoff = new PauseHandoff();
  const abort = deferred();
  const stopped = handoff.stop(() => abort.promise);
  let sent = false;
  const send = handoff.waitToSend().then((ready) => { sent = ready; });
  await Promise.resolve();
  assert.equal(handoff.stopping, true);
  assert.equal(handoff.queued, true);
  assert.equal(sent, false);
  abort.resolve();
  await Promise.all([stopped, send]);
  assert.equal(sent, true);
  assert.equal(handoff.stopping, false);
  assert.equal(handoff.queued, false);
});

test("暂停失败不放行下一条，并释放屏障", async () => {
  const handoff = new PauseHandoff();
  const abort = deferred();
  const stopped = handoff.stop(() => abort.promise);
  const waiting = handoff.waitToSend();
  const checks = Promise.all([
    assert.rejects(stopped, /暂停失败/),
    assert.rejects(waiting, /暂停失败/),
  ]);
  abort.reject(new Error("暂停失败"));
  await checks;
  assert.equal(handoff.stopping, false);
  assert.equal(handoff.queued, false);
});

test("等待期间不重复接受第二条提交", async () => {
  const handoff = new PauseHandoff();
  const abort = deferred();
  const stopped = handoff.stop(() => abort.promise);
  const first = handoff.waitToSend();
  assert.equal(await handoff.waitToSend(), false);
  abort.resolve();
  await stopped;
  assert.equal(await first, true);
});

test("再次暂停可以取消尚未发送的消息", async () => {
  const handoff = new PauseHandoff();
  const abort = deferred();
  const stopped = handoff.stop(() => abort.promise);
  const waiting = handoff.waitToSend();
  handoff.cancelQueued();
  abort.resolve();
  await stopped;
  assert.equal(await waiting, false);
});

test("取消后重新提交不会复活被取消的消息", async () => {
  const handoff = new PauseHandoff();
  const abort = deferred();
  const stopped = handoff.stop(() => abort.promise);
  const first = handoff.waitToSend();
  handoff.cancelQueued();
  const second = handoff.waitToSend();
  abort.resolve();
  await stopped;
  assert.equal(await first, false);
  assert.equal(await second, true);
});

test("重复暂停共享一次操作", async () => {
  const handoff = new PauseHandoff();
  const abort = deferred();
  let calls = 0;
  const first = handoff.stop(() => { calls++; return abort.promise; });
  const second = handoff.stop(() => { calls++; return abort.promise; });
  assert.equal(first, second);
  abort.resolve();
  await first;
  assert.equal(calls, 1);
  assert.equal(await handoff.waitToSend(), true);
});
