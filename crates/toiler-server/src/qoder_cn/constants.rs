//! Qoder 国内版（qoder.com.cn）协议常量。
//!
//! 对照 `pi-desktop/lib/qoder-cn/constants.ts`。这些数值逆向自官方 qodercli /
//! Qoder CN 桌面端，并与社区网关 qoder2api-hub 的国内版实现逐项对齐：`client_id`、
//! 网关域名、COSY 版本、`clienttype` 都参与服务端校验，改任何一项都必须同步服务端预期。

/// provider id：`models.json`、`auth.json`、`enabledModels` 里都用它做键。
pub const PROVIDER_ID: &str = "qoder-cn";

/// 「订阅」列表里的展示名。
pub const DISPLAY_NAME: &str = "Qoder CN";

/// 登录方式在订阅区的名字（pi 的 `oauth.name`）。
pub const LOGIN_LABEL: &str = "Qoder CN (subscription)";

/// 业务端点（纯 Bearer，无 COSY 签名）：设备码、刷新、身份。
pub const OPENAPI: &str = "https://openapi.qoder.com.cn";
/// 数据面网关（COSY 签名）：对话与模型清单。
pub const GATEWAY: &str = "https://gateway.qoder.com.cn";
/// 浏览器授权页所在站点。
pub const WEBSITE: &str = "https://qoder.com.cn";

/// 设备授权的 client_id（国内版专用，与国际版不同）。
pub const CLIENT_ID: &str = "1c5e33e1-364d-4ce6-b02c-acaa81274a5c";
/// 国内版设备授权 URL 会带 redirect_uri。
pub const REDIRECT_URI: &str = "qoder-work-cn://";
/// 业务接口的 User-Agent（官方 QoderWork/1.1.75）。
pub const USER_AGENT: &str = "QoderWork/1.1.75";
/// COSY 协议版本：`cosy-version` 头与签名 payload 共用同一个值。
pub const COSY_VERSION: &str = "1.1.75";
/// 推理请求的 UA 故意留空：官方 CLI 不设该头，由运行时补默认值。
pub const COSY_USER_AGENT: &str = "undici";
/// `cosy-clienttype`：上游按客户端身份决定模型授权，CLI 身份是 `5`。
pub const COSY_CLIENT_TYPE: &str = "5";
/// 签名头里固定上报的客户端 IP 形态（官方同款，非真实出口 IP）。
pub const CLIENT_IP: &str = "169.254.198.161";
/// 身份体里 `user_type` 为空时的兜底。
pub const DEFAULT_USER_TYPE: &str = "personal_professional_trial";

/// 对话入口：SSE 信封，响应体内层是标准 OpenAI chunk。
pub const CHAT_PATH: &str = "/algo/api/v2/service/pro/sse/agent_chat_generation\
?FetchKeys=llm_model_result&AgentId=agent_common&Encode=1";
/// 模型清单：同样是 COSY 签名，body 必须是 `encodeQoderBase64("{}")`。
pub const MODELS_PATH: &str = "/algo/api/v2/model/list?Encode=1";

pub const DEVICE_POLL_PATH: &str = "/api/v1/deviceToken/poll";
pub const DEVICE_REFRESH_PATH: &str = "/api/v1/deviceToken/refresh";
pub const USERINFO_PATH: &str = "/api/v1/userinfo";

/// 模型注册用的 baseUrl。
///
/// 注意：workman 里这个值**不是**常量 —— 它指向本机转发端点
/// （`http://127.0.0.1:<port>/qoder-cn/<token>`），因为 rpi 的 provider 不允许
/// 注入传输层，只能把请求打到自己的 HTTP 处理里再改写（等价于 pi 的
/// `withQoderCnFetch()`）。
pub const DEFAULT_MAX_OUTPUT_TOKENS: u64 = 32_768;

/// 设备码轮询节奏（官方 CLI：1s 间隔、5 分钟窗口）。
pub const POLL_INTERVAL_MS: u64 = 1_000;
pub const POLL_WINDOW_MS: u64 = 5 * 60 * 1_000;

/// 服务端 RSA 公钥：COSY 用它包裹本次会话的 AES 密钥（官方客户端硬编码）。
pub const SERVER_PUBLIC_KEY: &str = "-----BEGIN PUBLIC KEY-----
MIGfMA0GCSqGSIb3DQEBAQUAA4GNADCBiQKBgQDA8iMH5c02LilrsERw9t6Pv5Nc
4k6Pz1EaDicBMpdpxKduSZu5OANqUq8er4GM95omAGIOPOh+Nx0spthYA2BqGz+l
6HRkPJ7S236FZz73In/KVuLnwI8JJ2CbuJap8kvheCCZpmAWpb/cPx/3Vr/J6I17
XcW+ML9FoCI6AOvOzwIDAQAB
-----END PUBLIC KEY-----";

/// 官方国内版静态目录（顺序即官方 UI 顺序）。`key` 是上游真正的路由键。
#[derive(Debug, Clone, Copy)]
pub struct CatalogEntry {
    /// 上游路由键（`x-model-key` 头与 body 的 `model_config.key` 都用它）。
    pub key: &'static str,
    pub display_name: &'static str,
    pub vision: bool,
    pub reasoning: bool,
    /// 官方给出的最大输入 token（上下文窗口的保守默认值）。
    pub max_input_tokens: u64,
    /// 支持的思考档位（上游 effort 原值）。
    pub efforts: &'static [&'static str],
}

pub const STATIC_CATALOG: &[CatalogEntry] = &[
    CatalogEntry {
        key: "auto",
        display_name: "Auto",
        vision: true,
        reasoning: true,
        max_input_tokens: 200_000,
        efforts: &[],
    },
    CatalogEntry {
        key: "qmodel_38max",
        display_name: "Qwen3.8-Max",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &["low", "medium", "xhigh"],
    },
    CatalogEntry {
        key: "qfmodel",
        display_name: "Qwen3.8-Flash",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &["low", "medium", "xhigh"],
    },
    CatalogEntry {
        key: "qmodel_latest",
        display_name: "Qwen3.7-Max",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &[],
    },
    CatalogEntry {
        key: "qmodel",
        display_name: "Qwen3.7-Plus",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &[],
    },
    CatalogEntry {
        key: "q37fmodel",
        display_name: "Qwen3.7-Flash",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &[],
    },
    CatalogEntry {
        key: "dmodel",
        display_name: "DeepSeek-V4-Pro",
        vision: true,
        reasoning: true,
        max_input_tokens: 96_000,
        efforts: &["high", "max"],
    },
    CatalogEntry {
        key: "dfmodel",
        display_name: "DeepSeek-Flash",
        vision: true,
        reasoning: false,
        max_input_tokens: 180_000,
        efforts: &["low", "high", "max"],
    },
    CatalogEntry {
        key: "gmodel",
        display_name: "GLM-5.3",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &["low", "high", "max"],
    },
    CatalogEntry {
        key: "gfmodel",
        display_name: "GLM-5.3-Flash",
        vision: true,
        reasoning: true,
        max_input_tokens: 1_000_000,
        efforts: &["high", "max"],
    },
    CatalogEntry {
        key: "gm51model",
        display_name: "GLM-5.2",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &["high", "max"],
    },
    CatalogEntry {
        key: "kmodel_latest",
        display_name: "Kimi-K3",
        vision: true,
        reasoning: false,
        max_input_tokens: 180_000,
        efforts: &["low", "high", "max"],
    },
    CatalogEntry {
        key: "kmodel",
        display_name: "Kimi-K2.8-Preview",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &["low", "high", "max"],
    },
    CatalogEntry {
        key: "mmodel",
        display_name: "MiniMax-M2.7",
        vision: false,
        reasoning: false,
        max_input_tokens: 180_000,
        efforts: &[],
    },
    CatalogEntry {
        key: "lmodel",
        display_name: "QoDuck",
        vision: true,
        reasoning: true,
        max_input_tokens: 180_000,
        efforts: &["low", "high", "xhigh"],
    },
];

/// 按上游路由键取目录条目。
pub fn catalog_entry(key: &str) -> Option<&'static CatalogEntry> {
    STATIC_CATALOG.iter().find(|entry| entry.key == key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_catalog_matches_pi() {
        assert_eq!(STATIC_CATALOG.len(), 15);
        assert_eq!(STATIC_CATALOG[0].key, "auto");
        assert_eq!(catalog_entry("dmodel").unwrap().max_input_tokens, 96_000);
        assert!(catalog_entry("nope").is_none());
        assert_eq!(LOGIN_LABEL, "Qoder CN (subscription)");
    }
}
