//! 把**传输层中断**标记成 harness 认得的可重试错误。
//!
//! ## 为什么需要这一层
//!
//! 上游把流式连接掐断时（`Connection reset by peer (os error 54)` 一类），
//! `rpi-ai` 的 SSE 解码器把它包成 `AiError::Sse`，provider 发一个
//! `stop_reason = "error"` 的终态消息。此后 rpi 有两道重试闸门，**这道错误两道都过不去**：
//!
//! | 闸门 | 位置 | 为什么放行不了 |
//! |---|---|---|
//! | provider 层 | `retry_provider_request` | harness 把 `max_retries` 传成 `None`；而且它只重试 `AiError::Http`，`Sse` 直接透传 |
//! | harness 层 | `is_retryable_assistant_error` | 拿 `errorMessage` 做**子串**匹配，表里没有 `connection reset` / `reset by peer` 这类措辞 |
//!
//! 结果是一次网络抖动就整轮中断，用户得手点重发。
//!
//! ## 判定表是上游的共同缺口，不是移植退化
//!
//! 拿这条真实报错喂 pi 原版的 `RETRYABLE_PROVIDER_ERROR_PATTERN`
//! （`pi-ai/dist/utils/retry.js`）同样返回 false：它的表里有 `connection.?lost`、
//! `other side closed`、`network.?error`，但没有 `connection reset`。
//! 所以这不能靠「升级 rpi 修好」—— 0.22.1 的这份表逐字未变（实测 diff 为空）。
//!
//! ## 做法
//!
//! `rpi` 是 crates.io 依赖、不许改源码（AGENTS.md 铁律 1），所以在 **toiler 侧**
//! 包一层 [`Provider`]：透传 `id`/`models`，只在终态 `Error` 上给 `errorMessage`
//! 追加一段能命中 rpi 子串表的标记（[`RETRY_MARK`]）。harness 拿到之后按既有
//! `RetryPolicy`（默认指数退避、最多 10 次）自己重试，UI 走 `auto_retry_start`。
//!
//! **只覆盖传输层中断**：配额、参数错、鉴权失败这些确定性错误不加标记，
//! 否则会白等十轮退避。
//!
//! ## 与 pi 的差异
//!
//! | 项 | pi | toiler |
//! |---|---|---|
//! | 判定方式 | 正则（`connection.?lost` 这类允许中间夹字符） | 子串（rpi 的实现，无法改） |
//! | 覆盖范围 | 更宽（`terminated`、`unable to forward upstream` 等） | 只补传输层中断这一族 |
//! | 命中后的表现 | 报错原文进 `errorMessage` | 原文 + [`RETRY_MARK`] 后缀 |

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rpi_ai::event_stream::create_assistant_message_event_stream;
use rpi_ai::provider::DeferredProvider;
use rpi_ai::types::{AssistantMessage, AssistantMessageEvent, ErrorReason, StopReason};
use rpi_ai::{AssistantMessageEventStream, Context, Model, Provider, SimpleStreamOptions};

/// 追加到 `errorMessage` 末尾的标记。
///
/// 必须命中 rpi `is_retryable_assistant_error` 的子串表，`connection lost` 该项
/// 就在里面。选这种「方括号后缀」而不是改写整条文案，是因为最终失败时它是要
/// 展示给用户、并且会落进 v3 文件被 pi 读到的：原文必须留在最前面。
const RETRY_MARK: &str = "[connection lost]";

/// 传输层中断的特征词（一律小写，比对前把消息也转小写）。
///
/// 刻意**只**收「连接被掐断 / 流被截断」这一类。加进来之前先问一句：
/// 同样的请求原样重发一次，有可能成功吗？配额、参数、鉴权类问题的答案是「否」。
const TRANSPORT_MARKERS: &[&str] = &[
    // rpi SSE 解码器的两层包装（真实报错的第一行就是它）
    "error reading sse body",
    "error decoding response body",
    "error reading a body from connection",
    // 各类 socket 中断的系统措辞
    "connection reset", // macOS/Linux: Connection reset by peer
    "reset by peer",
    "os error 54",     // macOS ECONNRESET
    "os error 104",    // Linux ECONNRESET
    "os error 10054",  // Windows WSAECONNRESET
    "forcibly closed", // Windows 的措辞
    "connection aborted",
    "broken pipe",
    "econnreset",
    // 流在收全之前被截断（chunked 编码中途 EOF 会被当成正常结束）
    "unexpected eof",
    "incomplete message",
    "stream ended before",
];

/// 这条错误是不是「传输层中断」。
///
/// 纯函数、无 IO，方便单测直接钉住判定边界。
pub fn is_transport_interruption(message: &str) -> bool {
    let lower = message.to_lowercase();
    TRANSPORT_MARKERS.iter().any(|mark| lower.contains(mark))
}

/// 给可重试的传输层中断补上标记；不需要改就返回 `None`。
pub fn mark_transport_interruption(message: &str) -> Option<String> {
    if !is_transport_interruption(message) {
        return None;
    }
    // 已经标过（重试 N 次都失败时，同一条文案会被反复包装）就别再叠一层。
    if message.contains(RETRY_MARK) {
        return None;
    }
    Some(format!("{message} {RETRY_MARK}"))
}

/// 透传 `id`/`models`、只在终态错误上打标记的 [`Provider`] 包装。
///
/// 流是**重新转发**的：内层流逐帧转给外层 producer，取消语义因此保持原样
/// （外层 consumer 被 drop → 转发任务退出 → 内层流跟着 drop → 内层请求取消）。
pub struct RetryingProvider {
    inner: Arc<dyn Provider>,
}

impl RetryingProvider {
    pub fn new(inner: Arc<dyn Provider>) -> Self {
        Self { inner }
    }
}

/// 把一整个 provider 列表包起来。
///
/// harness 按 `model.provider` 找 provider（`find(|p| p.id() == model.provider)`），
/// 所以包装必须保持 `id()` 不变，否则模型全成了「没注册 provider」。
pub fn wrap_all(providers: &[Arc<dyn Provider>]) -> Vec<Arc<dyn Provider>> {
    providers
        .iter()
        .map(|provider| Arc::new(RetryingProvider::new(Arc::clone(provider))) as Arc<dyn Provider>)
        .collect()
}

#[async_trait::async_trait]
impl Provider for RetryingProvider {
    fn id(&self) -> &str {
        self.inner.id()
    }

    fn models(&self) -> &[Model] {
        self.inner.models()
    }

    /// 长轮询续传直接转给内层，不做装饰。
    ///
    /// 这一路的失败也是 `AssistantMessageEventStream`，理论上同样能打标记，但
    /// `deferred()` 返回的是借用（`&dyn DeferredProvider`），没法在这层包一个
    /// 新的 trait 对象出来。当前只有 rpi 的 faux provider 实现了它（测试用），
    /// 真实 provider 全部返回 `None`，所以先按「不装饰」处理并在文档里记一笔。
    fn deferred(&self) -> Option<&dyn DeferredProvider> {
        self.inner.deferred()
    }

    async fn stream_simple(
        &self,
        model: &Model,
        ctx: &Context,
        opts: &SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        let mut inner = self.inner.stream_simple(model, ctx, opts).await;
        let (mut producer, stream) = create_assistant_message_event_stream();

        // 内层流在没给终态的情况下结束时，得由我们补一个。
        //
        // 这条路径真实存在：SSE 连接被掐断时，若 chunked 编码在中途 EOF，
        // reqwest 会正常结束流而不是报错。不补的话 rpi 那边是
        // `AgentError::Provider("...ended without a terminal event")`——那是**不可
        // 重试**的 run 失败；补一个 Error 终态后它就走上重试（`ended without`
        // 本来就在 rpi 的可重试表里，说明上游也把它当瞬时故障）。
        let fallback = AssistantMessage::terminal(
            model.api.clone(),
            model.provider.clone(),
            model.id.clone(),
            StopReason::Error,
            "assistant-message event stream ended without a terminal event",
            now_ms(),
        );
        // 取消语义靠内层流传下来，但「没有终态」这个分支要自己区分中止与失败。
        let signal = opts.signal.clone();

        tokio::spawn(async move {
            while let Some(event) = inner.next().await {
                let terminal = event.is_terminal();
                if !producer.push(mark_event(event)).await {
                    return;
                }
                if terminal {
                    return;
                }
            }
            let error = if signal.is_cancelled() {
                let mut aborted = fallback;
                aborted.stop_reason = StopReason::Aborted;
                aborted.error_message = Some("Request aborted".to_string());
                aborted
            } else {
                fallback
            };
            let reason = if signal.is_cancelled() {
                ErrorReason::Aborted
            } else {
                ErrorReason::Error
            };
            let _ = producer
                .push(AssistantMessageEvent::Error { reason, error })
                .await;
        });

        stream
    }
}

/// 只动终态 `Error` 里的 `errorMessage`，别的帧原样转发。
fn mark_event(event: AssistantMessageEvent) -> AssistantMessageEvent {
    match event {
        AssistantMessageEvent::Error { reason, mut error } => {
            if let Some(marked) = error
                .error_message
                .as_deref()
                .and_then(mark_transport_interruption)
            {
                error.error_message = Some(marked);
            }
            AssistantMessageEvent::Error { reason, error }
        }
        other => other,
    }
}

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实报错（用户会话文件里那一行逐字）。
    const REAL: &str = "sse error: error reading sse body: error decoding response body \
(caused by: request or response body error; error reading a body from connection; \
Connection reset by peer (os error 54))";

    #[test]
    fn real_world_reset_is_marked_and_becomes_retryable() {
        let marked = mark_transport_interruption(REAL).expect("这条真实报错必须被标记");
        // 原文必须留在最前面：它要展示给用户、也会落进 v3 文件给 pi 读。
        assert!(marked.starts_with(REAL), "{marked}");
        assert!(marked.ends_with(RETRY_MARK), "{marked}");
        // 标记必须真的能被 harness 认出来，否则这一层就是白写的。
        let message = AssistantMessage::terminal(
            rpi_ai::Api::AnthropicMessages,
            "apisets",
            "deepseek-v4.1-flash",
            StopReason::Error,
            marked.clone(),
            0,
        );
        assert!(
            rpi_harness::compaction::is_retryable_assistant_error(&message),
            "标记后仍不可重试：{marked}"
        );
    }

    #[test]
    fn unmarked_message_stays_non_retryable() {
        // 前半句：不改之前确实是不可重试的（这就是这个模块存在的理由）。
        let message = AssistantMessage::terminal(
            rpi_ai::Api::AnthropicMessages,
            "apisets",
            "deepseek-v4.1-flash",
            StopReason::Error,
            REAL,
            0,
        );
        assert!(!rpi_harness::compaction::is_retryable_assistant_error(
            &message
        ));
    }

    #[test]
    fn deterministic_errors_are_left_alone() {
        // 配额 / 参数 / 鉴权类：重发一次还是同样的结果，不该打标记。
        for message in [
            "provider error [insufficient_quota]: You exceeded your current quota",
            "auth error: invalid api key",
            "schema error: tool arguments did not match the schema",
            "provider error [invalid_request_error]: max_tokens must be positive",
        ] {
            assert_eq!(mark_transport_interruption(message), None, "{message}");
        }
    }

    #[test]
    fn marking_is_idempotent() {
        let once = mark_transport_interruption(REAL).unwrap();
        // 重试 N 次都失败时会反复经过这里，标记不该越叠越长。
        assert_eq!(mark_transport_interruption(&once), None);
    }

    #[test]
    fn windows_and_linux_reset_wording_is_covered() {
        for message in [
            "error reading a body from connection: Connection reset by peer (os error 104)",
            "An existing connection was forcibly closed by the remote host. (os error 10054)",
            "read tcp: connection reset by peer",
            "broken pipe (os error 32)",
        ] {
            assert!(
                mark_transport_interruption(message).is_some(),
                "没覆盖到：{message}"
            );
        }
    }

    #[test]
    fn abort_events_pass_through_untouched() {
        // 用户主动中止不属于「可重试」，reason 与文案都不能动。
        let aborted = AssistantMessage::terminal(
            rpi_ai::Api::AnthropicMessages,
            "apisets",
            "deepseek-v4.1-flash",
            StopReason::Aborted,
            "Connection reset by peer (os error 54)",
            0,
        );
        match mark_event(AssistantMessageEvent::Error {
            reason: ErrorReason::Aborted,
            error: aborted,
        }) {
            AssistantMessageEvent::Error { reason, error } => {
                assert_eq!(reason, ErrorReason::Aborted);
                // 这里是刻意保留的已知差异：只按文案判定，不按 reason 判定。
                // 中止的文案若恰好含网络措辞（provider 在 abort 竞态里发的是传输
                // 错误），打上标记也不会触发重试——harness 的 `will_retry` 还要求
                // `stop_reason == Error`，而 Aborted 不是。
                assert!(error.error_message.unwrap().contains(RETRY_MARK));
            }
            other => panic!("类型被改写了：{other:?}"),
        }
    }

    #[test]
    fn non_error_events_are_forwarded_verbatim() {
        let message = AssistantMessage::terminal(
            rpi_ai::Api::AnthropicMessages,
            "apisets",
            "deepseek-v4.1-flash",
            StopReason::Stop,
            "ok",
            0,
        );
        match mark_event(AssistantMessageEvent::Done {
            reason: rpi_ai::types::DoneReason::Stop,
            message,
        }) {
            AssistantMessageEvent::Done { reason, message } => {
                assert_eq!(reason, rpi_ai::types::DoneReason::Stop);
                assert_eq!(message.error_message.as_deref(), Some("ok"));
            }
            other => panic!("类型被改写了：{other:?}"),
        }
    }

    #[test]
    fn wrapping_keeps_provider_ids_resolvable() {
        // harness 按 `model.provider == provider.id()` 解析，包装后必须仍然对得上。
        let inner: Arc<dyn Provider> = Arc::new(ScriptedProvider::new("apisets", Ok("hi"), None));
        let wrapped = wrap_all(&[inner]);
        assert_eq!(wrapped.len(), 1);
        assert_eq!(wrapped[0].id(), "apisets");
        assert_eq!(wrapped[0].models().len(), 1);
        assert_eq!(wrapped[0].models()[0].id, "deepseek-v4.1-flash");
    }

    #[tokio::test]
    async fn terminal_error_from_inner_stream_gets_marked() {
        let provider: Arc<dyn Provider> =
            Arc::new(ScriptedProvider::new("apisets", Err(REAL), None));
        let wrapped = RetryingProvider::new(provider);
        let model = test_model();
        let opts = SimpleStreamOptions::default();
        let stream = wrapped
            .stream_simple(&model, &Context::new(Vec::new()), &opts)
            .await;

        let error = stream.result().await.expect("必须有终态");
        assert_eq!(error.stop_reason, StopReason::Error);
        let message = error.error_message.clone().unwrap_or_default();
        assert!(message.starts_with(REAL), "{message}");
        assert!(message.ends_with(RETRY_MARK), "{message}");
        assert!(rpi_harness::compaction::is_retryable_assistant_error(
            &error
        ));
    }

    #[tokio::test]
    async fn successful_stream_is_forwarded_unchanged() {
        let provider: Arc<dyn Provider> =
            Arc::new(ScriptedProvider::new("apisets", Ok("hello"), None));
        let wrapped = RetryingProvider::new(provider);
        let opts = SimpleStreamOptions::default();
        let stream = wrapped
            .stream_simple(&test_model(), &Context::new(Vec::new()), &opts)
            .await;

        let done = stream.result().await.expect("必须有终态");
        assert_eq!(done.stop_reason, StopReason::Stop);
        assert!(done.error_message.is_none(), "{done:?}");
    }

    #[tokio::test]
    async fn a_stream_that_ends_without_terminal_becomes_a_retryable_error() {
        // chunked 编码中途 EOF 时 reqwest 会「正常」结束流，provider 却没发终态。
        // 不补一个 Error，rpi 那边就是不可重试的 run 失败。
        let provider: Arc<dyn Provider> =
            Arc::new(ScriptedProvider::new("apisets", Ok("hi"), Some(())));
        let wrapped = RetryingProvider::new(provider);
        let stream = wrapped
            .stream_simple(
                &test_model(),
                &Context::new(Vec::new()),
                &SimpleStreamOptions::default(),
            )
            .await;

        let error = stream.result().await.expect("必须有终态");
        assert_eq!(error.stop_reason, StopReason::Error);
        assert!(error
            .error_message
            .clone()
            .unwrap_or_default()
            .contains("ended without"));
        assert!(rpi_harness::compaction::is_retryable_assistant_error(
            &error
        ));
    }

    fn test_model() -> Model {
        Model::new(
            "deepseek-v4.1-flash".to_string(),
            "DeepSeek".to_string(),
            rpi_ai::Api::AnthropicMessages,
            "apisets".to_string(),
            "https://example.test".to_string(),
        )
    }

    /// 只会「按剧本发文本，或发一个 Error」的假 provider。
    ///
    /// 用自造的而不是 `FauxProvider`：这里要精确控制终态的**文案**（标记的输入
    /// 就是文案），faux 的脚本 API 表达不了这个。
    struct ScriptedProvider {
        /// provider id（harness 按 `model.provider` 找它）。
        provider_id: String,
        model: Model,
        outcome: ScriptedOutcome,
        omit_terminal: bool,
    }

    enum ScriptedOutcome {
        Text(String),
        Error(String),
    }

    impl ScriptedProvider {
        fn new(id: &str, outcome: Result<&str, &str>, omit_terminal: Option<()>) -> Self {
            let mut model = test_model();
            model.provider = id.to_string();
            Self {
                provider_id: id.to_string(),
                model,
                outcome: match outcome {
                    Ok(text) => ScriptedOutcome::Text(text.to_string()),
                    Err(message) => ScriptedOutcome::Error(message.to_string()),
                },
                omit_terminal: omit_terminal.is_some(),
            }
        }
    }

    #[async_trait::async_trait]
    impl Provider for ScriptedProvider {
        fn id(&self) -> &str {
            &self.provider_id
        }

        fn models(&self) -> &[Model] {
            std::slice::from_ref(&self.model)
        }

        async fn stream_simple(
            &self,
            model: &Model,
            _ctx: &Context,
            opts: &SimpleStreamOptions,
        ) -> AssistantMessageEventStream {
            let (mut producer, stream) =
                rpi_ai::event_stream::create_assistant_message_event_stream_with_cancellation(
                    &opts.signal,
                );
            let model = model.clone();
            let outcome = match &self.outcome {
                ScriptedOutcome::Text(text) => ScriptedOutcome::Text(text.clone()),
                ScriptedOutcome::Error(message) => ScriptedOutcome::Error(message.clone()),
            };
            let omit_terminal = self.omit_terminal;
            tokio::spawn(async move {
                match outcome {
                    ScriptedOutcome::Text(text) => {
                        let mut message = AssistantMessage::terminal(
                            model.api.clone(),
                            model.provider.clone(),
                            model.id.clone(),
                            StopReason::Stop,
                            "",
                            0,
                        );
                        message.error_message = None;
                        message.content =
                            vec![rpi_ai::types::Content::Text(rpi_ai::types::TextContent {
                                kind: Default::default(),
                                text,
                                text_signature: None,
                            })];
                        if !omit_terminal {
                            let _ = producer
                                .push(AssistantMessageEvent::Done {
                                    reason: rpi_ai::types::DoneReason::Stop,
                                    message,
                                })
                                .await;
                        }
                    }
                    ScriptedOutcome::Error(message) => {
                        let error = AssistantMessage::terminal(
                            model.api.clone(),
                            model.provider.clone(),
                            model.id.clone(),
                            StopReason::Error,
                            message,
                            0,
                        );
                        let _ = producer
                            .push(AssistantMessageEvent::Error {
                                reason: ErrorReason::Error,
                                error,
                            })
                            .await;
                    }
                }
            });
            stream
        }
    }
}
