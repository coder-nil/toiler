/**
 * 设备指纹派生。
 *
 * 上游风控按「一台设备」聚类：同一个账号长期来自同一台机器是正常形态，
 * 不同账号共用一个指纹则会被关联。所以 COSY 头里的 machine 系列字段
 * 一律**按账号 UID 单向派生**，而不是随机生成（随机值会让同一账号每
 * 请求换设备，同样触发风控）。
 *
 * 派生规则与 qoder2api-hub 国内版一致：
 *   machineId    md5("<salt>:<uid>") 改造成 UUIDv4 形态（版本位 4、变体位 a）
 *   machineType  machineId 去横线后取前 18 位
 *   machineToken sha512("machinetoken:<uid>") 的 base64url 前 43 位
 *   hostname     "DESKTOP-" + md5("hostname:<uid>") 前 7 位大写
 */
import { createHash } from "node:crypto";

/** 稳定的 UUIDv4 形态派生值（同一 uid + salt 永远相同）。 */
export function deriveDeviceId(uid: string, salt: string): string {
  const hash = createHash("md5").update(`${salt}:${uid || "anonymous"}`).digest("hex");
  // 伪造 UUIDv4 的版本位（第 13 位）与变体位（第 17 位），与官方 machineId 同形。
  const uuidish = `${hash.slice(0, 12)}4${hash.slice(13, 16)}a${hash.slice(17)}`;
  return [
    uuidish.slice(0, 8),
    uuidish.slice(8, 12),
    uuidish.slice(12, 16),
    uuidish.slice(16, 20),
    uuidish.slice(20),
  ].join("-");
}

export function deriveMachineType(uid: string): string {
  return deriveDeviceId(uid, "machinetype").replace(/-/g, "").slice(0, 18);
}

export function deriveMachineToken(uid: string): string {
  const raw = createHash("sha512").update(`machinetoken:${uid}`).digest();
  return raw.toString("base64url").slice(0, 43);
}

export function deriveMachineHostname(uid: string): string {
  const hash = createHash("md5").update(`hostname:${uid || "anonymous"}`).digest("hex");
  return `DESKTOP-${hash.slice(0, 7).toUpperCase()}`;
}

/**
 * cosy-machineos：官方按运行系统上报 {arch}_{platform}。
 *
 * 这里保留官方默认的 x86_64_linux 形态（与 qoder2api-hub 的默认值一致）：
 * 账号一旦按某个形态建档，换形态等于换设备，没必要跟随宿主机漂移。
 */
export const QODER_CN_MACHINE_OS = "x86_64_linux";
