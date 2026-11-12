/**
 * COSY 请求签名（Qoder 数据面的鉴权方式）。
 *
 * 上游不认单纯的 Bearer token：每个请求都要带一套 COSY 头，其中
 *
 *   authorization = "Bearer COSY.<payload>.<sig>"
 *   payload       = base64(sorted-compact JSON{cosyVersion, ideVersion, info, requestId, version})
 *   info          = base64(AES-128-CBC(identity JSON, key=iv=tempKey))
 *   cosy-key      = base64(RSA-PKCS1v15(tempKey))
 *   sig           = md5(payload \n cosy-key \n date \n body \n path)
 *   path          = URL path，去掉 "/algo" 前缀
 *
 * 也就是说签名覆盖**请求体的最终字节**：body 任何变化都必须重新签，
 * 不能先签名再改 body（这也是请求体必须先 encodeQoderBase64 再签的原因）。
 *
 * 会话对象按 access token 存活：token 轮换后 info 里的 security_oauth_token
 * 必须换新，因此整份会话重建。
 */
import { createCipheriv, createHash, publicEncrypt, constants, randomUUID } from "node:crypto";
import {
  QODER_CN,
  QODER_CN_SERVER_PUBLIC_KEY,
} from "./constants";
import {
  QODER_CN_MACHINE_OS,
  deriveDeviceId,
  deriveMachineHostname,
  deriveMachineToken,
  deriveMachineType,
} from "./fingerprint";

/** 上游缺省用户类型（身份体里 user_type 为空时的兜底，与官方一致）。 */
export const QODER_DEFAULT_USER_TYPE = "personal_professional_trial";

export interface QoderCosyIdentity {
  uid: string;
  nickname?: string;
  accessToken: string;
  refreshToken?: string;
  userType?: string;
  organizationId?: string;
  organizationName?: string;
}

export interface QoderCosyHeadersOptions {
  /** 上游模型的 Qoder key（如 `qmodel`）；留空表示不带 x-model-key（模型清单）。 */
  modelKey?: string;
  /** 是否按 SSE 场景附带 cache-control / machinehostname。 */
  sse?: boolean;
  accept?: string;
}

export interface QoderCosySession {
  readonly uid: string;
  readonly accessToken: string;
  readonly machineId: string;
  /** 对一次请求体签名并返回完整请求头。 */
  headers(body: string, rawUrl: string, options?: QoderCosyHeadersOptions): Record<string, string>;
}

/**
 * 键排序 + 无空白的紧凑 JSON。
 *
 * 服务端解出的 identity 字节要与客户端签的完全一致，所以这里不能用
 * `JSON.stringify(obj)`（键序不可控），必须显式排序。
 */
export function sortedCompactJson(entries: Record<string, unknown>): Buffer {
  const keys = Object.keys(entries).sort();
  const parts = keys.map((key) => {
    const value = entries[key] === undefined || entries[key] === null ? "" : entries[key];
    return `${JSON.stringify(key)}:${JSON.stringify(value)}`;
  });
  return Buffer.from(`{${parts.join(",")}}`, "utf8");
}

/** 从 URL 取签名用的 path（去掉 "/algo" 前缀，与官方一致）。 */
export function signaturePath(rawUrl: string): string {
  let path = "/";
  try {
    path = new URL(rawUrl).pathname || "/";
  } catch {
    path = rawUrl.split("?")[0] || "/";
  }
  return path.startsWith("/algo") ? path.slice("/algo".length) : path;
}

export function createCosySession(identity: QoderCosyIdentity): QoderCosySession {
  const uid = identity.uid || "";
  const accessToken = identity.accessToken || "";
  if (!accessToken) throw new Error("qoder cosy: empty access token");

  // 会话密钥：官方取 uuid4().hex[:16]，也就是 16 个十六进制字符当 ASCII 用，
  // AES 的 key 与 iv 是同一串字节。长度和字符集都不要「加固」——服务端解出的
  // 字节必须逐字节相同。
  const tempKey = randomUUID().replace(/-/g, "").slice(0, 16);
  const keyBytes = Buffer.from(tempKey, "utf8");
  const cosyKey = publicEncrypt(
    { key: QODER_CN_SERVER_PUBLIC_KEY, padding: constants.RSA_PKCS1_PADDING },
    keyBytes,
  ).toString("base64");

  const plainIdentity = sortedCompactJson({
    name: identity.nickname || "",
    aid: uid,
    uid,
    yx_uid: "",
    organization_id: identity.organizationId || "",
    organization_name: identity.organizationName || "",
    user_type: identity.userType || QODER_DEFAULT_USER_TYPE,
    security_oauth_token: accessToken,
    refresh_token: identity.refreshToken || "",
  });
  const cipher = createCipheriv("aes-128-cbc", keyBytes, keyBytes);
  const info = Buffer.concat([cipher.update(plainIdentity), cipher.final()]).toString("base64");

  const machineId = deriveDeviceId(uid, "machine");
  const machineToken = deriveMachineToken(uid);
  const machineType = deriveMachineType(uid);
  const machineHostname = deriveMachineHostname(uid);

  const sign = (body: string, rawUrl: string) => {
    const payload = {
      cosyVersion: QODER_CN.cosyVersion,
      ideVersion: "",
      info,
      requestId: randomUUID(),
      version: "v1",
    };
    const payloadB64 = sortedCompactJson(payload).toString("base64");
    const date = String(Math.floor(Date.now() / 1000));
    const raw = [payloadB64, cosyKey, date, body, signaturePath(rawUrl)].join("\n");
    const sig = createHash("md5").update(raw, "utf8").digest("hex");
    return { date, bearer: `Bearer COSY.${payloadB64}.${sig}` };
  };

  return {
    uid,
    accessToken,
    machineId,
    headers(body, rawUrl, options = {}) {
      const { date, bearer } = sign(body, rawUrl);
      const sse = options.sse ?? true;
      const headers: Record<string, string> = {
        "cosy-data-policy": "agree",
        "content-type": "application/json",
        "cosy-business-product": "cli",
        "cosy-business-type": "agent",
        "cosy-scene": "assistant",
        "cosy-clienttype": QODER_CN.cosyClientType,
        "cosy-date": date,
        "cosy-user": uid,
        "cosy-key": cosyKey,
        accept: options.accept ?? (sse ? "text/event-stream" : "application/json"),
        authorization: bearer,
        "accept-encoding": "identity",
        "cosy-version": QODER_CN.cosyVersion,
        "cosy-machineid": machineId,
        "cosy-machinetoken": machineToken,
        "cosy-machinetype": machineType,
        "cosy-clientip": QODER_CN.cosyClientIp,
        "login-version": "v2",
        "user-agent": QODER_CN.cosyUserAgent,
        "cosy-machineos": QODER_CN_MACHINE_OS,
      };
      if (sse) {
        headers["cosy-machinehostname"] = machineHostname;
        headers["cache-control"] = "no-cache";
      }
      if (options.modelKey) {
        headers["x-model-key"] = options.modelKey;
        headers["x-model-source"] = "system";
      }
      return headers;
    },
  };
}
