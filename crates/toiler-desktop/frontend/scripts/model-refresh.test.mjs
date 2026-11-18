import assert from "node:assert/strict";
import test from "node:test";
import { readFile } from "node:fs/promises";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { refreshProviderModels } = await jiti.import("../lib/model-discovery.ts");

test("刷新远端目录只发送供应商 id，不读取或发送已有 API Key", async () => {
  const controller = new AbortController();
  const models = [{ id: "new-model", name: "新模型" }];
  const result = await refreshProviderModels("openai", async (url, options) => {
    assert.equal(url, "/api/models-config/refresh");
    assert.equal(options.method, "POST");
    assert.deepEqual(JSON.parse(options.body), { providerName: "openai" });
    assert.equal(options.signal, controller.signal);
    return Response.json({ models });
  }, controller.signal);
  assert.deepEqual(result, models);
});

test("刷新失败、响应格式错误、断网和取消均不会被当成刷新成功", async () => {
  for (const response of [
    Response.json({ error: "上游拒绝访问" }, { status: 502 }),
    Response.json({}, { status: 200 }),
    Response.json({ error: "刷新正忙" }, { status: 409 }),
  ]) {
    await assert.rejects(refreshProviderModels("openai", async () => response));
  }
  await assert.rejects(refreshProviderModels("openai", async () => { throw new Error("网络不可用"); }), /网络不可用/);
  await assert.rejects(refreshProviderModels("openai", async () => { throw new DOMException("已取消", "AbortError"); }), { name: "AbortError" });
});

test("模型设置保留再次刷新入口，并在成功后通知模型列表重载", async () => {
  const source = await readFile(new URL("../components/ModelsConfig.tsx", import.meta.url), "utf8");
  assert.ok(source.includes('t("models.refreshProviderModels")'));
  assert.ok(!source.includes('{discoveryState.phase !== "success" && ('), "自定义导入成功后不能隐藏刷新入口");
  const oauth = source.slice(source.indexOf("function OAuthDetail"), source.indexOf("function ApiKeyDetail"));
  const apiKey = source.slice(source.indexOf("function ApiKeyDetail"), source.indexOf("function faviconUrl"));
  assert.ok(oauth.includes("<ProviderModelsPicker"));
  assert.ok(oauth.includes("configured={provider.loggedIn}"));
  assert.ok(apiKey.includes("<ProviderModelsPicker"));
  assert.ok(apiKey.includes("configured={provider.configured}"));
  const detail = await readFile(new URL("../components/ProviderModelsPicker.tsx", import.meta.url), "utf8");
  assert.ok(detail.includes("await refreshProviderModels(providerId"));
  assert.ok(detail.includes("setRefreshTick((tick) => tick + 1)"));
  assert.ok(detail.includes("onChanged?.()"));
  assert.ok(detail.includes("reloadKey={`${configured}:${reloadKey}:${refreshTick}`}"));
  assert.ok(detail.includes("abortRef.current?.abort()"));
  assert.ok(!detail.includes("const disabled = !configured"), "未登录 / 未保存 Key 时也要能点，让服务端说明原因");
  assert.ok(detail.includes('t("models.refreshProviderModels")'), "图标缺失无障碍名称");
  assert.ok(detail.includes('aria-label={t("models.refreshProviderModels")}'));
  assert.ok(detail.includes("<RefreshCw"));
  assert.ok(detail.includes('"animate-spin"'));
  const picker = await readFile(new URL("../components/EnabledModelsPicker.tsx", import.meta.url), "utf8");
  const counter = picker.indexOf('t("models.visibleSelectedCount"');
  const refreshIcon = picker.indexOf("{countAction}");
  const otherActions = picker.indexOf('{saving && <span');
  assert.ok(counter < refreshIcon && refreshIcon < otherActions, "刷新图标必须紧跟已选数量，而不是出现在全选操作之后");
});

test("上游发现到的模型与勾选列表合并，刷新入口在已选数量之后", async () => {
  const picker = await readFile(new URL("../components/EnabledModelsPicker.tsx", import.meta.url), "utf8");
  // 待导入模型与已导入模型在同一个列表容器里渲染：这是「合并」的判据。
  const listStart = picker.indexOf('<div style={{ display: "flex", flexDirection: "column", maxHeight: 260');
  const listEnd = picker.indexOf("{state.phase === \"ready\" && state.warnings.length > 0");
  assert.ok(listStart > 0 && listEnd > listStart);
  const list = picker.slice(listStart, listEnd);
  assert.ok(list.includes("visibleModels.map"), "已导入模型在列表里");
  assert.ok(list.includes("visiblePending.map"), "待导入模型也在同一个列表里");
  assert.ok(list.includes('t("models.pendingNew")'));
  assert.ok(list.includes("onImportPending"));

  // 刷新入口（countAction）仍紧跟「已选 N/M」。
  const counter = picker.indexOf('t("models.visibleSelectedCount"');
  const refreshIcon = picker.indexOf("{countAction}");
  const otherActions = picker.indexOf("{saving && <span");
  assert.ok(counter < refreshIcon && refreshIcon < otherActions);

  // 自定义 Provider 详情里不再有独立的「导入模型」区块，而是把发现结果交给勾选列表。
  const source = await readFile(new URL("../components/ModelsConfig.tsx", import.meta.url), "utf8");
  const detail = source.slice(source.indexOf("function ProviderDetail"), source.indexOf("// ── ThinkingLevelMap editor"));
  assert.ok(!detail.includes('t("models.discoveryAddSelected")'), "不应再有独立的「添加所选模型」按钮");
  assert.ok(!detail.includes("discoverySelectShown"), "不应再有独立的选择当前结果工具栏");
  assert.ok(detail.includes("pendingModels={pendingModels}"));
  assert.ok(detail.includes("onImportPending={importPendingModel}"));
  assert.ok(detail.includes("countAction={("), "刷新图标作为 countAction 传入");
  assert.ok(detail.includes("<RefreshCw"));
});

test("导入并设为可见是一次点击：先写 models.json 再写 enabledModels", async () => {
  const source = await readFile(new URL("../components/ModelsConfig.tsx", import.meta.url), "utf8");
  const start = source.indexOf("const importModelAsVisible = useCallback");
  assert.ok(start > 0, "必须有 importModelAsVisible");
  const body = source.slice(start, source.indexOf("const handleSave = useCallback(async () => {\n    setSaving(true);", start));
  const putModels = body.indexOf('method: "PUT"');
  const putEnabled = body.indexOf('"/api/models-config/enabled"');
  assert.ok(putModels > 0 && putEnabled > putModels, "必须先写 models.json、再写可见范围");
  assert.ok(body.includes("providerName"), "可见范围按 provider 写入");
  assert.ok(body.includes("modelIds"), "把当前勾选 + 新模型一起提交");
});
