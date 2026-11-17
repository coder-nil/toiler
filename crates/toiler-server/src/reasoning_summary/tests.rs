use super::*;
use rpi_ai::types::{AssistantMessage, AssistantMessageEvent, Content, DoneReason, StopReason};
use tower::ServiceExt;

fn model() -> Model {
    let mut model = Model::new(
        "test",
        "测试",
        Api::OpenaiResponses,
        "custom",
        "https://example.invalid/v1",
    );
    model.reasoning = true;
    model
}

/// 下拉框里的每个非 off 档位都要请求摘要，不限于 high。
#[test]
fn every_non_off_level_requests_summary() {
    for level in [
        ThinkingLevel::Minimal,
        ThinkingLevel::Low,
        ThinkingLevel::Medium,
        ThinkingLevel::High,
        ThinkingLevel::Xhigh,
        ThinkingLevel::Max,
    ] {
        let opts = SimpleStreamOptions {
            reasoning: Some(level),
            ..Default::default()
        };
        assert!(wants_summary(&model(), &opts), "{level:?}");
    }
    for level in [None, Some(ThinkingLevel::Off)] {
        let opts = SimpleStreamOptions {
            reasoning: level,
            ..Default::default()
        };
        assert!(!wants_summary(&model(), &opts));
    }
}

#[test]
fn other_protocols_and_non_reasoning_models_are_unchanged() {
    let opts = SimpleStreamOptions {
        reasoning: Some(ThinkingLevel::High),
        ..Default::default()
    };
    let mut model = model();
    model.reasoning = false;
    assert!(!wants_summary(&model, &opts));
    model.reasoning = true;
    for api in [Api::AnthropicMessages, Api::OpenaiCompletions] {
        model.api = api;
        assert!(!wants_summary(&model, &opts));
    }
}

/// 不修改 effort、工具参数、历史推理签名或缓存参数；已有 summary 统一为 auto。
#[test]
fn adding_summary_only_changes_one_field() {
    for effort in ["minimal", "low", "medium", "high", "xhigh", "max"] {
        let original = json!({
            "reasoning": {"effort": effort, "other": true}, "store": false,
            "input": [{"type": "reasoning", "encrypted_content": "opaque"}],
            "tools": [{"type": "function", "name": "read"}], "prompt_cache_key": "session"
        });
        let mut body = original.clone();
        add_summary(&mut body).unwrap();
        assert_eq!(body["reasoning"]["summary"], "auto");
        body["reasoning"].as_object_mut().unwrap().remove("summary");
        assert_eq!(body, original);
    }
    let mut body = json!({"reasoning": {"summary": "concise"}});
    add_summary(&mut body).unwrap();
    assert_eq!(body["reasoning"]["summary"], "auto");
}

/// thinkingLevelMap 可以让非 off 档位省略 effort，仍必须请求摘要。
#[test]
fn no_effort_does_not_prevent_requesting_summary() {
    let mut body = json!({"model": "test", "input": []});
    add_summary(&mut body).unwrap();
    assert_eq!(body["reasoning"], json!({"summary": "auto"}));
    assert!(add_summary(&mut json!([])).is_err());
    assert!(add_summary(&mut json!({"reasoning": "invalid"})).is_err());
}

#[tokio::test]
async fn forward_request_keeps_auth_and_rebuilds_body_length() {
    let relay = SummaryRelay::default();
    let target = Target {
        url: "https://example.invalid/v1/responses".into(),
        signal: CancellationToken::new(),
    };
    let body = json!({"reasoning": {"effort": "low"}, "input": "测试", "stream": true});
    let request = Request::builder()
        .method("POST")
        .header("authorization", "Bearer fixture-not-a-secret")
        .header("x-custom-auth", "fixture")
        .header("host", "127.0.0.1:12345")
        .header("content-length", "1")
        .header("connection", "x-hop")
        .header("x-hop", "remove")
        .body(Body::from(body.to_string()))
        .unwrap();
    let forwarded = prepare_request(&relay.http, &target, request)
        .await
        .unwrap();
    assert_eq!(forwarded.url().as_str(), target.url);
    assert_eq!(
        forwarded.headers()["authorization"],
        "Bearer fixture-not-a-secret"
    );
    assert_eq!(forwarded.headers()["x-custom-auth"], "fixture");
    for header in ["host", "content-length", "connection", "x-hop"] {
        assert!(!forwarded.headers().contains_key(header), "{header}");
    }
    let sent: Value =
        serde_json::from_slice(forwarded.body().unwrap().as_bytes().unwrap()).unwrap();
    assert_eq!(
        sent["reasoning"],
        json!({"effort": "low", "summary": "auto"})
    );
    assert_eq!(sent["input"], body["input"]);
}

#[test]
fn targets_are_per_request_and_revoked_on_drop() {
    let relay = Arc::new(SummaryRelay::default());
    let signal = CancellationToken::new();
    let first = relay.register("https://example.invalid/v1/", signal.clone());
    let second = relay.register("https://other.invalid/", CancellationToken::new());
    assert_ne!(first.token, second.token);
    assert_eq!(
        relay.targets.lock().unwrap()[&first.token].url,
        "https://example.invalid/v1/responses"
    );
    assert_eq!(
        relay.targets.lock().unwrap()[&second.token].url,
        "https://other.invalid/v1/responses"
    );
    drop(first);
    assert!(signal.is_cancelled());
    assert_eq!(relay.targets.lock().unwrap().len(), 1);
    drop(second);
    assert!(relay.targets.lock().unwrap().is_empty());
}

type Observations = Arc<Mutex<Vec<(String, Option<ThinkingLevel>)>>>;

struct Stub {
    id: String,
    models: Vec<Model>,
    seen: Observations,
    hold: bool,
}

#[async_trait::async_trait]
impl Provider for Stub {
    fn id(&self) -> &str {
        &self.id
    }
    fn models(&self) -> &[Model] {
        &self.models
    }
    async fn stream_simple(
        &self,
        model: &Model,
        _ctx: &Context,
        opts: &SimpleStreamOptions,
    ) -> AssistantMessageEventStream {
        self.seen
            .lock()
            .unwrap()
            .push((model.base_url.clone(), opts.reasoning));
        let (mut producer, stream) =
            create_assistant_message_event_stream_with_cancellation(&opts.signal);
        let hold = self.hold;
        tokio::spawn(async move {
            if hold {
                producer.cancellation_token().cancelled().await;
                return;
            }
            let mut message = AssistantMessage::terminal(
                Api::OpenaiResponses,
                "custom",
                "test",
                StopReason::ToolUse,
                "",
                0,
            );
            message.content = vec![
                Content::thinking("摘要"),
                Content::tool_call("call1", "read", json!({"path": "a"})),
            ];
            let partial = Arc::new(message.clone());
            let Content::ToolCall(call) = &message.content[1] else {
                unreachable!()
            };
            for event in [
                AssistantMessageEvent::ThinkingStart {
                    content_index: 0,
                    partial: partial.clone(),
                },
                AssistantMessageEvent::ThinkingDelta {
                    content_index: 0,
                    delta: "摘要".into(),
                    partial: partial.clone(),
                },
                AssistantMessageEvent::ThinkingEnd {
                    content_index: 0,
                    content: "摘要".into(),
                    partial: partial.clone(),
                },
                AssistantMessageEvent::ToolCallStart {
                    content_index: 1,
                    partial: partial.clone(),
                },
                AssistantMessageEvent::ToolCallEnd {
                    content_index: 1,
                    tool_call: call.clone(),
                    partial,
                },
                AssistantMessageEvent::Done {
                    reason: DoneReason::ToolUse,
                    message,
                },
            ] {
                if !producer.push(event).await {
                    break;
                }
            }
        });
        stream
    }
}

async fn assert_empty_soon(relay: &SummaryRelay) {
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        while !relay.targets.lock().unwrap().is_empty() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

/// 思考增量必须在工具调用前到达，不可等 Done 才整块显示；不改原模型地址。
#[tokio::test]
async fn summary_events_are_forwarded_before_tool_calls() {
    let relay = Arc::new(SummaryRelay::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    for provider_id in ["custom", "apisets"] {
        let mut model = model();
        model.provider = provider_id.into();
        let inner = Arc::new(Stub {
            id: provider_id.into(),
            models: vec![model.clone()],
            seen: seen.clone(),
            hold: false,
        });
        let wrapped = relay.wrap_all(&[inner], 12345).remove(0);
        assert_eq!(wrapped.models()[0].base_url, model.base_url);
        let opts = SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::Low),
            ..Default::default()
        };
        let mut stream = wrapped
            .stream_simple(&model, &Context::new(vec![]), &opts)
            .await;
        let mut types = Vec::new();
        while let Some(event) = stream.next().await {
            types.push(event.type_tag().to_string());
            if let AssistantMessageEvent::ThinkingDelta { delta, .. } = event {
                assert_eq!(delta, "摘要");
            }
        }
        assert_eq!(
            types,
            [
                "thinking_start",
                "thinking_delta",
                "thinking_end",
                "toolcall_start",
                "toolcall_end",
                "done"
            ]
        );
        assert!(seen
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .0
            .starts_with("http://127.0.0.1:12345/reasoning-summary/"));
        assert_eq!(
            seen.lock().unwrap().last().unwrap().1,
            Some(ThinkingLevel::Low)
        );
        assert_eq!(model.base_url, "https://example.invalid/v1");
        assert_empty_soon(&relay).await;
    }
}

#[tokio::test]
async fn off_bypasses_relay_and_cancel_or_drop_releases_registration() {
    let relay = Arc::new(SummaryRelay::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let inner = Arc::new(Stub {
        id: "custom".into(),
        models: vec![model()],
        seen: seen.clone(),
        hold: true,
    });
    let wrapped = relay.wrap_all(&[inner], 12345).remove(0);
    let opts = SimpleStreamOptions::default();
    let stream = wrapped
        .stream_simple(&model(), &Context::new(vec![]), &opts)
        .await;
    assert_eq!(seen.lock().unwrap()[0].0, model().base_url);
    assert!(relay.targets.lock().unwrap().is_empty());
    drop(stream);
    for cancel in [false, true] {
        let opts = SimpleStreamOptions {
            reasoning: Some(ThinkingLevel::High),
            ..Default::default()
        };
        let stream = wrapped
            .stream_simple(&model(), &Context::new(vec![]), &opts)
            .await;
        assert_eq!(relay.targets.lock().unwrap().len(), 1);
        if cancel {
            opts.signal.cancel();
        } else {
            drop(stream);
            assert_empty_soon(&relay).await;
            continue;
        }
        assert_empty_soon(&relay).await;
        drop(stream);
    }
}

fn state() -> AppState {
    let dir = std::env::temp_dir().join(format!("toiler-summary-{}", uuid::Uuid::new_v4()));
    let catalog =
        crate::catalog::ModelCatalog::from_config(json!({"providers": {}}), None).unwrap();
    AppState::with_cwd(dir.join("sessions"), catalog, &dir).with_stores(
        crate::auth::AuthStore::new(dir.join("auth.json")),
        crate::models_json::ModelsFile::at(dir.join("models.json")),
        crate::settings::SettingsFile::at(dir.join("settings.json")),
        crate::projects::ProjectStore::at(dir.join("work.sqlite")),
        crate::trust::TrustStore::at(dir.join("trust.json")),
    )
}

async fn route_status(state: &AppState, token: &str, peer: Option<&str>, body: &str) -> StatusCode {
    let mut request = Request::builder()
        .method("POST")
        .uri(format!("/reasoning-summary/{token}/v1/responses"))
        .body(Body::from(body.to_string()))
        .unwrap();
    if let Some(peer) = peer {
        request
            .extensions_mut()
            .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    }
    crate::routes::router(state.clone())
        .oneshot(request)
        .await
        .unwrap()
        .status()
}

/// 内部端点不能成为手机端或浏览器可滥用的任意地址代理。
#[tokio::test]
async fn route_rejects_remote_missing_expired_tokens_and_invalid_body() {
    let state = state();
    let registration = state
        .inner
        .summary_relay
        .register("https://example.invalid/v1", CancellationToken::new());
    let token = registration.token.clone();
    assert_eq!(
        route_status(&state, &token, Some("192.0.2.1:1"), "{}").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        route_status(&state, &token, None, "{}").await,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        route_status(&state, "unknown", Some("127.0.0.1:1"), "{}").await,
        StatusCode::FORBIDDEN
    );
    for peer in ["127.0.0.1:1", "[::1]:1", "[::ffff:127.0.0.1]:1"] {
        assert_eq!(
            route_status(&state, &token, Some(peer), "invalid").await,
            StatusCode::BAD_REQUEST
        );
    }
    drop(registration);
    assert_eq!(
        route_status(&state, &token, Some("127.0.0.1:1"), "{}").await,
        StatusCode::FORBIDDEN
    );
}

/// 请求被取消时，在收到上游首个响应头之前也应立即退出。
#[tokio::test]
async fn cancelled_request_does_not_connect_upstream() {
    let state = state();
    let signal = CancellationToken::new();
    let registration = state
        .inner
        .summary_relay
        .register("https://example.invalid/v1", signal.clone());
    signal.cancel();
    assert_eq!(
        route_status(&state, &registration.token, Some("127.0.0.1:1"), "{}").await,
        StatusCode::REQUEST_TIMEOUT
    );
}

/// 透明传递 SSE 字节与 429/Retry-After，不缓存整段，不把失败伪装成 200。
#[tokio::test]
async fn response_stream_keeps_chunks_status_and_retry_headers() {
    let (sender, receiver) = tokio::sync::mpsc::channel::<Result<bytes::Bytes, std::io::Error>>(4);
    let incoming = futures::stream::unfold(receiver, |mut receiver| async {
        receiver.recv().await.map(|item| (item, receiver))
    });
    let upstream = reqwest::Response::from(
        axum::http::Response::builder()
            .status(StatusCode::TOO_MANY_REQUESTS)
            .header("retry-after", "5")
            .header("content-type", "text/event-stream")
            .header("content-length", "999")
            .body(reqwest::Body::wrap_stream(incoming))
            .unwrap(),
    );
    let signal = CancellationToken::new();
    let response = streaming_response(upstream, signal.clone());
    assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(response.headers()["retry-after"], "5");
    assert!(!response.headers().contains_key("content-length"));
    let mut body = response.into_body().into_data_stream();
    let chunk = bytes::Bytes::from_static(
        b"data: {\"type\":\"response.reasoning_summary_text.delta\",\"delta\":\"part\"}\n\n",
    );
    sender.send(Ok(chunk.clone())).await.unwrap();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), body.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(first, chunk);
    signal.cancel();
    assert!(body.next().await.is_none());
    drop(sender);
}

#[test]
fn redirects_cannot_bypass_the_registered_target() {
    let upstream = reqwest::Response::from(
        axum::http::Response::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header("location", "https://other.invalid/responses")
            .body(reqwest::Body::from(""))
            .unwrap(),
    );
    let response = streaming_response(upstream, CancellationToken::new());
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    assert!(!response.headers().contains_key("location"));
}

/// 新建与续聊都走 dialogue_catalog；覆盖自定义 provider 和内置 Coding，
/// 但供 /api/models 与 oneshot 使用的共享目录仍持有原来的 provider 和地址。
#[test]
fn dialogue_catalog_wraps_custom_and_builtin_without_changing_shared_catalog() {
    let state = state().with_binding("127.0.0.1", 12345);
    let catalog = crate::catalog::ModelCatalog::from_config(json!({"providers": {
        "custom": {"api": "openai-responses", "baseUrl": "https://example.invalid/v1", "apiKey": "fixture",
            "models": [{"id": "test", "reasoning": true}]}
    }}), None).unwrap().with_apisets("fixture", &[json!({
        "id": "test-builtin", "api": "openai-responses", "reasoning": true
    })]);
    state.replace_catalog(catalog);
    let shared = state.catalog();
    let live = state.dialogue_catalog();
    assert_eq!(shared.providers.len(), 2);
    for (original, wrapped) in shared.providers.iter().zip(&live.providers) {
        assert_eq!(original.id(), wrapped.id());
        assert_eq!(original.models(), wrapped.models());
        assert!(!Arc::ptr_eq(original, wrapped));
    }
    for (original, current) in shared.providers.iter().zip(&state.catalog().providers) {
        assert!(Arc::ptr_eq(original, current));
    }
    assert_eq!(shared.models, live.models);
}
