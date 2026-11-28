import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const { listenForNativeEditorHistory } = await createJiti(import.meta.url).import("../lib/editor-history.ts");

function nativeAction(target, action) {
  return target.dispatchEvent(new CustomEvent("toiler:editor-history", { cancelable: true, detail: action }));
}

test("原生菜单优先使用当前编辑器的历史，未接管时保留普通输入框撤销", (t) => {
  const window = new EventTarget();
  const previous = Object.getOwnPropertyDescriptor(globalThis, "window");
  Object.defineProperty(globalThis, "window", { configurable: true, value: window });
  t.after(() => previous ? Object.defineProperty(globalThis, "window", previous) : Reflect.deleteProperty(globalThis, "window"));

  let active = false;
  const actions = [];
  const remove = listenForNativeEditorHistory((action) => {
    if (!active) return false;
    actions.push(action);
    return true;
  });
  assert.equal(nativeAction(window, "undo"), true, "普通输入框应走默认命令");
  active = true;
  assert.equal(nativeAction(window, "undo"), false);
  assert.equal(nativeAction(window, "redo"), false);
  assert.equal(nativeAction(window, "unknown"), true);
  remove();
  assert.equal(nativeAction(window, "undo"), true, "关闭编辑器后不再截获");
  assert.deepEqual(actions, ["undo", "redo"]);
});
