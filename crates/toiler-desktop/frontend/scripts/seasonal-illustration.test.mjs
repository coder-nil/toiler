import assert from "node:assert/strict";
import test from "node:test";
import { createJiti } from "jiti";

const jiti = createJiti(import.meta.url);
const { parseSeasonSnapshot, seasonRefreshDelay } = await jiti.import("../lib/seasonal-illustration.ts");

test("拒绝未知季节、原型字段和已过期的交节时间，避免显示错误资源", () => {
  const valid = { season: "winter", serverTime: 1000, nextChangeAt: 2000 };
  assert.deepEqual(parseSeasonSnapshot(valid), valid);
  for (const invalid of [
    null,
    { ...valid, season: "toString" },
    { ...valid, season: "__proto__" },
    { ...valid, season: "rain" },
    { ...valid, serverTime: -1 },
    { ...valid, serverTime: "1000" },
    { ...valid, nextChangeAt: 1000 },
    { ...valid, nextChangeAt: NaN },
    { ...valid, nextChangeAt: Infinity },
  ]) assert.equal(parseSeasonSnapshot(invalid), null);
});

test("临近交节时扣除请求耗时，到点刷新且不受浏览器本地日期影响", () => {
  const snapshot = { season: "winter", serverTime: 1000, nextChangeAt: 8000 };
  assert.equal(seasonRefreshDelay(snapshot, 1500), 5500);
  assert.equal(seasonRefreshDelay(snapshot, 8000), 250);
  assert.equal(seasonRefreshDelay({ ...snapshot, nextChangeAt: 1_000_000 }, 0), 60_000);
});
