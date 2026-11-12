/**
 * Qoder 国内版（qoder.com.cn）协议常量。
 *
 * 这些数值逆向自官方 qodercli / Qoder CN 桌面端，并与社区网关
 * qoder2api-hub 的国内版实现逐项对齐：client_id、网关域名、COSY 版本、
 * clienttype 都参与服务端校验，改任何一项都必须同步服务端预期。
 *
 * 区域差异（国内 / 国际）只在这里体现，业务代码不出现硬编码域名。
 */

/** provider id：models.json、auth.json、enabledModels 里都用它做键。 */
export const QODER_CN_PROVIDER_ID = "qoder-cn";

/** 「订阅」列表里的展示名。 */
export const QODER_CN_DISPLAY_NAME = "Qoder CN";

/** 登录方式在订阅区的名字（OAuthAuth.loginLabel 同源）。 */
export const QODER_CN_LOGIN_LABEL = "Sign in with Qoder CN";

export const QODER_CN = {
  /** 业务端点（纯 Bearer，无 COSY 签名）：设备码、刷新、身份。 */
  openapi: "https://openapi.qoder.com.cn",
  /** 数据面网关（COSY 签名）：对话与模型清单。 */
  gateway: "https://gateway.qoder.com.cn",
  /** 浏览器授权页所在站点。 */
  website: "https://qoder.com.cn",
  /** 设备授权的 client_id（国内版专用，与国际版不同）。 */
  clientId: "1c5e33e1-364d-4ce6-b02c-acaa81274a5c",
  /** 国内版设备授权 URL 会带 redirect_uri。 */
  redirectUri: "qoder-work-cn://",
  /** 业务接口的 User-Agent（官方 QoderWork/1.1.75）。 */
  userAgent: "QoderWork/1.1.75",
  /**
   * COSY 协议版本：cosy-version 头与签名 payload 共用同一个值。
   * 官方客户端升级后这里要跟着升，否则上游会以「版本过低」拒绝。
   */
  cosyVersion: "1.1.75",
  /**
   * 推理请求的 UA 故意留空：官方 CLI 不设该头，由 undici 补默认值。
   * 改成浏览器 UA 反而是额外的「网关特征」。
   */
  cosyUserAgent: "undici",
  /** cosy-clienttype：上游按客户端身份决定模型授权，CLI 身份是 "5"。 */
  cosyClientType: "5",
  /** 签名头里固定上报的客户端 IP 形态（官方同款，非真实出口 IP）。 */
  cosyClientIp: "169.254.198.161",
} as const;

/** 对话入口：SSE 信封，响应体内层是标准 OpenAI chunk。 */
export const QODER_CN_CHAT_PATH =
  "/algo/api/v2/service/pro/sse/agent_chat_generation"
  + "?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";

/** 模型清单：同样是 COSY 签名，body 必须是 qoderEncode("{}")。 */
export const QODER_CN_MODELS_PATH = "/algo/api/v2/model/list?Encode=1";

export const QODER_CN_DEVICE_POLL_PATH = "/api/v1/deviceToken/poll";
export const QODER_CN_DEVICE_REFRESH_PATH = "/api/v1/deviceToken/refresh";
export const QODER_CN_USERINFO_PATH = "/api/v1/userinfo";
export const QODER_CN_PLAN_PATH = "/api/v2/user/plan";
export const QODER_CN_QUOTA_PATH = "/api/v2/quota/usage";

/**
 * 模型注册用的 baseUrl。
 *
 * 真实请求由 `lib/qoder-cn/transport.ts` 整体改写（URL、body、COSY 头），
 * 所以这个地址不会被真正拨号；写成普通 OpenAI 形态是为了让模型面板和
 * models.json 里的展示保持可读。
 */
export const QODER_CN_BASE_URL = "https://gateway.qoder.com.cn/v1";

/** 设备码轮询节奏（官方 CLI：1s 间隔、5 分钟窗口）。 */
export const QODER_CN_POLL_INTERVAL_MS = 1_000;
export const QODER_CN_POLL_WINDOW_MS = 5 * 60 * 1_000;

/** 服务端 RSA 公钥：COSY 用它包裹本次会话的 AES 密钥（官方客户端硬编码）。 */
export const QODER_CN_SERVER_PUBLIC_KEY = `-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END PUBLIC KEY-----`;
