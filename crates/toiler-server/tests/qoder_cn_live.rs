//! `qoder-cn` 的真实链路验证（需要网络 + 已登录的 Qoder CN 账号）。
//!
//! 默认 `#[ignore]`：CI 与 `cargo test` 都不会跑它。手工验证方式：
//!
//! ```bash
//! # 1. 起服务（debug 日志会打出本机转发端点地址，含路径令牌）
//! RUST_LOG=toiler_server=debug cargo run -p toiler-server -- --port 8892
//! # 2. 把端点地址喂给这个测试
//! QODER_CN_ENDPOINT=http://127.0.0.1:8892/qoder-cn/<token> \
//!   cargo test -p toiler-server --test qoder_cn_live -- --ignored --nocapture
//! ```
//!
//! 它验证的是**接缝**：rpi 的 `OpenAiCompletionsProvider` 能不能消费我们代理出来的
//! 标准 chunk 流（`[DONE]`、usage、reasoning_content、finish_reason）。
//! 上游协议本身（COSY 签名、信封解包）由 `qoder_cn::transport` 的单元测试守着。

use std::sync::Arc;

use toiler_server::{oneshot, qoder_cn};

#[tokio::test]
#[ignore = "需要网络与已登录的 Qoder CN 账号；见文件头注释"]
async fn provider_streams_a_real_qoder_cn_completion() {
    let Ok(base_url) = std::env::var("QODER_CN_ENDPOINT") else {
        panic!("请设置 QODER_CN_ENDPOINT（形如 http://127.0.0.1:8892/qoder-cn/<token>）");
    };
    let models = qoder_cn::models(&base_url);
    let model = models
        .iter()
        .find(|model| model.id == "gfmodel")
        .expect("gfmodel 在内置目录里")
        .clone();
    let provider: Arc<dyn rpi_ai::Provider> = Arc::new(qoder_cn::provider(&base_url));

    let reply = oneshot::complete(
        provider,
        &model,
        None,
        "只回复两个字：收到".to_string(),
        512,
        std::time::Duration::from_secs(180),
    )
    .await
    .expect("完成一次补全");

    println!("text={:?}", reply.text);
    println!("stop_reason={:?}", reply.stop_reason);
    println!("usage={:?}", reply.usage);
    assert!(!reply.text.trim().is_empty(), "上游必须有文本增量");
}
