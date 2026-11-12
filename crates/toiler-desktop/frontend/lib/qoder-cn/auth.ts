/**
 * Qoder CN 登录与状态刷新（OAuth 设备码 + PKCE S256）。
 *
 * 链路与官方 qodercli / 国内版客户端一致：
 *
 *   1. 本地生成 verifier，challenge = base64url(sha256(verifier))
 *   2. 浏览器打开 `{website}/device/selectAccounts?challenge=…&challenge_method=S256
 *      &nonce=…&redirect_uri=…&client_id=…&machine_id=…`
 *   3. 轮询 `GET {openapi}/api/v1/deviceToken/poll?nonce=&verifier=&challenge_method=S256`
 *      —— 404 / 202 表示「用户还没点确认」，200 才拿到 token
 *   4. token 过期前用 `POST /api/v1/deviceToken/refresh {refresh_token}` 续期
 *
 * 设备码这条链路**不带账号凭证**，因此走纯 Bearer 的 openapi，不参与 COSY 签名。
 */
import { createHash, randomBytes, randomUUID } from "node:crypto";
import { readFile } from "node:fs/promises";
import { homedir } from "node:os";
import path from "node:path";
import {
  QODER_CN,
  QODER_CN_DEVICE_POLL_PATH,
  QODER_CN_DEVICE_REFRESH_PATH,
  QODER_CN_POLL_INTERVAL_MS,
  QODER_CN_POLL_WINDOW_MS,
  QODER_CN_USERINFO_PATH,
} from "./constants";
import { QODER_DEFAULT_USER_TYPE } from "./cosy";

const PKCE_ALPHABET = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-._~";
const REQUEST_TIMEOUT_MS = 30_000;

/** 身份字段（存进 auth.json 的 oauth 凭证，也用于构造 COSY 身份体）。 */
export interface QoderCnIdentityFields {
  uid: string;
  nickname: string;
  userType: string;
  organizationId: string;
  organizationName: string;
}

export interface QoderCnTokenSet {
  access: string;
  refresh: string;
  /** 毫秒时间戳。 */
  expires: number;
}

export interface QoderCnDeviceLogin {
  authUrl: string;
  /** 轮询直到用户完成授权；404/202 视为仍在等待。 */
  poll(signal: AbortSignal): Promise<QoderCnTokenSet & QoderCnIdentityFields>;
}

function requestSignal(signal: AbortSignal): AbortSignal {
  return AbortSignal.any([AbortSignal.timeout(REQUEST_TIMEOUT_MS), signal]);
}

function sleep(ms: number, signal: AbortSignal): Promise<void> {
  return new Promise((resolve, reject) => {
    if (signal.aborted) {
      reject(signal.reason instanceof Error ? signal.reason : new Error("aborted"));
      return;
    }
    const timer = setTimeout(() => {
      signal.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(timer);
      reject(signal.reason instanceof Error ? signal.reason : new Error("aborted"));
    };
    signal.addEventListener("abort", onAbort, { once: true });
  });
}

function makePkce(): { verifier: string; challenge: string } {
  const raw = randomBytes(64);
  let verifier = "";
  for (const byte of raw) verifier += PKCE_ALPHABET[byte % PKCE_ALPHABET.length];
  const challenge = createHash("sha256").update(verifier, "ascii").digest("base64url");
  return { verifier, challenge };
}

/**
 * 设备授权 URL 里的 machine_id：官方客户端读自己的 `~/.qoder-cn/.auth/machine_id`，
 * 没有就临时生成。这里沿用同一规则，让「本机已装 Qoder CN」的环境看起来一致。
 */
async function resolveLoginMachineId(): Promise<string> {
  for (const candidate of [
    path.join(homedir(), ".qoder-cn", ".auth", "machine_id"),
    path.join(homedir(), ".qoder", ".auth", "machine_id"),
  ]) {
    try {
      const value = (await readFile(candidate, "utf8")).trim();
      if (value) return value;
    } catch {
      // 文件不存在是常态：继续找下一个来源。
    }
  }
  return randomUUID();
}

function deviceExpiry(payload: Record<string, unknown>): number {
  const expiresIn = Number(payload.expires_in);
  if (Number.isFinite(expiresIn) && expiresIn > 0) {
    // 上游给的是毫秒（官方实现按 /1000 处理）。
    return Date.now() + expiresIn;
  }
  const expiresAt = typeof payload.expires_at === "string" ? Date.parse(payload.expires_at) : Number.NaN;
  if (Number.isFinite(expiresAt)) return expiresAt;
  return Date.now() + 30 * 24 * 3600 * 1000;
}

function stringField(payload: Record<string, unknown>, ...keys: string[]): string {
  for (const key of keys) {
    const value = payload[key];
    if (typeof value === "string" && value) return value;
  }
  return "";
}

async function readJson(response: Response): Promise<Record<string, unknown> | null> {
  try {
    const parsed: unknown = await response.json();
    return parsed && typeof parsed === "object" && !Array.isArray(parsed)
      ? parsed as Record<string, unknown>
      : null;
  } catch {
    return null;
  }
}

/** 拉取账号身份（uid / 昵称 / 用户类型 / 组织）。失败时返回空字段，不阻塞登录。 */
export async function fetchQoderCnIdentity(
  accessToken: string,
  signal: AbortSignal,
): Promise<Partial<QoderCnIdentityFields>> {
  try {
    const response = await fetch(`${QODER_CN.openapi}${QODER_CN_USERINFO_PATH}`, {
      method: "GET",
      headers: {
        Accept: "application/json",
        "User-Agent": QODER_CN.userAgent,
        Authorization: `Bearer ${accessToken}`,
      },
      signal: requestSignal(signal),
    });
    if (!response.ok) return {};
    const payload = await readJson(response);
    if (!payload) return {};
    return {
      uid: stringField(payload, "id"),
      nickname: stringField(payload, "name"),
      userType: stringField(payload, "user_type"),
      organizationId: stringField(payload, "organization_id", "organizationId"),
      organizationName: stringField(payload, "organization_name", "organizationName"),
    };
  } catch {
    return {};
  }
}

/** 发起一次设备码登录，返回浏览器授权地址与轮询器。 */
export async function startQoderCnDeviceLogin(): Promise<QoderCnDeviceLogin> {
  const { verifier, challenge } = makePkce();
  const nonce = randomUUID();
  const machineId = await resolveLoginMachineId();

  const query = new URLSearchParams({
    challenge,
    challenge_method: "S256",
    nonce,
    redirect_uri: QODER_CN.redirectUri,
    client_id: QODER_CN.clientId,
    machine_id: machineId,
  });
  const authUrl = `${QODER_CN.website}/device/selectAccounts?${query.toString()}`;

  return {
    authUrl,
    async poll(signal) {
      const pollQuery = new URLSearchParams({
        nonce,
        verifier,
        challenge_method: "S256",
      });
      const pollUrl = `${QODER_CN.openapi}${QODER_CN_DEVICE_POLL_PATH}?${pollQuery.toString()}`;
      const deadline = Date.now() + QODER_CN_POLL_WINDOW_MS;

      while (Date.now() < deadline) {
        signal.throwIfAborted();
        const response = await fetch(pollUrl, {
          method: "GET",
          headers: { Accept: "application/json", "User-Agent": QODER_CN.userAgent },
          signal: requestSignal(signal),
        });
        // 404 / 202：用户还没在浏览器里点确认，继续等。
        if (response.status === 404 || response.status === 202) {
          await sleep(QODER_CN_POLL_INTERVAL_MS, signal);
          continue;
        }
        if (!response.ok) {
          const text = await response.text().catch(() => "");
          throw new Error(
            `Qoder CN 设备授权轮询失败：HTTP ${response.status}${text ? ` ${text.slice(0, 200)}` : ""}`,
          );
        }
        const payload = await readJson(response);
        const access = stringField(payload ?? {}, "token", "device_token");
        if (!access) {
          // 200 但还没 token：上游偶发，按仍待授权处理。
          await sleep(QODER_CN_POLL_INTERVAL_MS, signal);
          continue;
        }
        const identity = await fetchQoderCnIdentity(access, signal);
        return {
          access,
          refresh: stringField(payload ?? {}, "refresh_token"),
          expires: deviceExpiry(payload ?? {}),
          uid: identity.uid ?? "",
          nickname: identity.nickname ?? "",
          userType: identity.userType || QODER_DEFAULT_USER_TYPE,
          organizationId: identity.organizationId ?? "",
          organizationName: identity.organizationName ?? "",
        };
      }
      throw new Error("Qoder CN 登录超时：请在浏览器完成授权后重试");
    },
  };
}

/** 用 refresh_token 换新的 access token（上游字段同设备码响应）。 */
export async function refreshQoderCnToken(
  refreshToken: string,
  signal: AbortSignal,
): Promise<QoderCnTokenSet> {
  if (!refreshToken) throw new Error("Qoder CN 凭证缺少 refresh_token，请重新登录");
  const response = await fetch(`${QODER_CN.openapi}${QODER_CN_DEVICE_REFRESH_PATH}`, {
    method: "POST",
    headers: {
      "Content-Type": "application/json",
      Accept: "application/json",
      "User-Agent": QODER_CN.userAgent,
    },
    body: JSON.stringify({ refresh_token: refreshToken }),
    signal: requestSignal(signal),
  });
  if (!response.ok) {
    const text = await response.text().catch(() => "");
    // TOKEN_EXPIRE / 12153：上游已吊销离线会话，刷新没有意义，只能重新登录。
    const dead = /TOKEN_EXPIRE|12153|Offline user session not found/.test(text);
    throw new Error(
      dead
        ? "Qoder CN 会话已失效（TOKEN_EXPIRE），请重新登录"
        : `Qoder CN token 刷新失败：HTTP ${response.status}${text ? ` ${text.slice(0, 200)}` : ""}`,
    );
  }
  const payload = await readJson(response);
  if (!payload) throw new Error("Qoder CN token 刷新返回了非 JSON 响应");
  const access = stringField(payload, "token", "device_token");
  if (!access) throw new Error("Qoder CN token 刷新未返回 token");
  return {
    access,
    refresh: stringField(payload, "refresh_token") || refreshToken,
    expires: deviceExpiry(payload),
  };
}
