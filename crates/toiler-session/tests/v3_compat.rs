//! v3 兼容性测试：用**真实形状**的 fixture 跑完整链路。
//!
//! 覆盖阶段 1 的验收标准：
//! 1. 能加载 v3 文件（含分支 rewind、compaction、pi 私有条目）
//! 2. 能追加一轮对话，且**老字节一个不改**
//! 3. 追加后的文件能被重新加载（pi 也能读，因为格式没变）

use std::path::{Path, PathBuf};
use std::sync::Arc;

use rpi_agent::message::AgentMessage;
use rpi_harness::session::types::{EntryQuery, SessionRepo, SessionStorage};
use rpi_harness::session::Session;
use workman_session::{V3SessionRepo, V3SessionStorage};

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

/// 把 fixture 复制到临时目录，测试之间互不影响。
fn stage(name: &str, tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "workman-session-{}-{}-{}",
        std::process::id(),
        tag,
        name.replace('.', "-")
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let dest = dir.join(name);
    std::fs::copy(fixture(name), &dest).unwrap();
    dest
}

fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap()
        .block_on(fut)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

#[test]
fn loads_basic_session() {
    let storage = V3SessionStorage::load(fixture("basic.jsonl")).unwrap();
    let report = storage.load_report();
    assert_eq!(report.skipped, Vec::<String>::new(), "不应跳过任何行");
    assert_eq!(report.loaded_entries, 9);

    let stats = block_on(storage.get_stats()).unwrap();
    // system + user + assistant + toolResult + assistant
    assert_eq!(stats.message_count, 5);

    // pi 私有条目（session_info / context_edit）被折叠成 Custom 条目，没丢
    let customs = block_on(storage.find_entries(&EntryQuery {
        entry_type: Some("custom"),
        ..Default::default()
    }))
    .unwrap();
    let types: Vec<_> = customs
        .iter()
        .filter_map(|e| match e {
            rpi_harness::session::types::Entry::Custom(c) => Some(c.custom_type.clone()),
            _ => None,
        })
        .collect();
    assert!(types.contains(&"session_info".to_string()), "{types:?}");
    assert!(types.contains(&"context_edit".to_string()), "{types:?}");

    // 会话名从 session_info 恢复到了 rpi 的 fact 上
    assert_eq!(
        block_on(storage.get_name()).unwrap().as_deref(),
        Some("演示会话")
    );
}

#[test]
fn loads_branchy_session_with_rewind() {
    let storage = V3SessionStorage::load(fixture("branch.jsonl")).unwrap();
    let report = storage.load_report();
    assert_eq!(report.skipped, Vec::<String>::new(), "分支应当自动回退叶子");
    assert_eq!(report.loaded_entries, 9);

    // 9 个条目里有 8 条是 message（含 bashExecution → Custom message）
    let stats = block_on(storage.get_stats()).unwrap();
    assert_eq!(stats.message_count, 8);
}

#[test]
fn append_preserves_existing_bytes_and_reloads() {
    let path = stage("basic.jsonl", "append");
    let before = std::fs::read(&path).unwrap();

    let storage = V3SessionStorage::load(&path).unwrap();
    let loaded_entries = storage.load_report().loaded_entries;
    let session = Session::new(Arc::new(storage), None);

    let user = rpi_ai::types::UserMessage::new("追加一条验收消息", now_ms());
    let user_id = block_on(session.append_message(AgentMessage::User(user))).unwrap();
    assert!(!user_id.is_empty());

    let after = std::fs::read(&path).unwrap();
    assert!(after.len() > before.len(), "文件应该变长");
    assert_eq!(
        &after[..before.len()],
        &before[..],
        "已有字节必须一字不改（append-only）"
    );

    // 追加的行是合法 v3：能重新加载，且条数 +1
    let reloaded = V3SessionStorage::load(&path).unwrap();
    assert_eq!(reloaded.load_report().skipped, Vec::<String>::new());
    assert_eq!(reloaded.load_report().loaded_entries, loaded_entries + 1);

    let text = std::fs::read_to_string(&path).unwrap();
    let last = text.lines().last().unwrap();
    assert!(last.contains(r#""role":"user""#), "{last}");
    assert!(!last.contains(r#""seq""#), "v3 行不能带 seq: {last}");
    assert!(last.contains(r#""content":[{"type":"text""#), "{last}");
    // 时间戳必须是 ISO 字符串（pi 的写法）
    assert!(last.contains("Z\""), "{last}");
}

#[test]
fn appended_messages_keep_stop_reason_in_pi_spelling() {
    let path = stage("basic.jsonl", "stopreason");
    let storage = V3SessionStorage::load(&path).unwrap();
    let session = Session::new(Arc::new(storage), None);

    // 用 assistant 形态的消息触发 stopReason 写回路径
    let assistant = rpi_ai::types::AssistantMessage {
        role: Default::default(),
        content: vec![rpi_ai::types::Content::text("好")],
        api: rpi_ai::types::Api::OpenaiCompletions,
        provider: rpi_ai::types::ProviderId::from("qoder"),
        model: "GLM-5.3".into(),
        response_model: None,
        response_id: None,
        usage: rpi_ai::types::Usage {
            input: 1,
            output: 1,
            cache_read: 0,
            cache_write: 0,
            cache_write_1h: None,
            reasoning: Some(0),
            total_tokens: 2,
            cost: Default::default(),
        },
        stop_reason: rpi_ai::types::StopReason::ToolUse,
        deferred: None,
        error_message: None,
        raw_stop_reason: None,
        end_turn: None,
        timestamp: now_ms(),
    };
    block_on(session.append_message(AgentMessage::Assistant(Box::new(assistant)))).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    let last = text.lines().last().unwrap();
    assert!(
        last.contains(r#""stopReason":"toolUse""#),
        "必须写回 pi 的 camelCase：{last}"
    );
}

#[test]
fn malformed_line_is_skipped_not_fatal() {
    let path = stage("basic.jsonl", "broken");
    let mut text = std::fs::read_to_string(&path).unwrap();
    text.push_str("这不是 JSON\n");
    text.push_str(r#"{"type":"message","id":"zzz","parentId":"e19d9cc7","timestamp":"2026-11-10T06:42:13.000Z","message":{"role":"user","content":[{"type":"text","text":"坏行之后的正常消息"}],"timestamp":1794292933000}}"#);
    text.push('\n');
    std::fs::write(&path, text).unwrap();

    let storage = V3SessionStorage::load(&path).unwrap();
    let report = storage.load_report();
    assert_eq!(report.skipped.len(), 1, "{:?}", report.skipped);
    assert_eq!(report.loaded_entries, 10, "坏行后面的行仍要加载");
}

#[test]
fn set_name_writes_a_pi_readable_session_info_entry() {
    let path = stage("basic.jsonl", "name");
    let storage = V3SessionStorage::load(&path).unwrap();
    block_on(storage.set_name(Some("重命名后的会话"))).unwrap();

    let text = std::fs::read_to_string(&path).unwrap();
    let last = text.lines().last().unwrap();
    assert!(last.contains(r#""type":"session_info""#), "{last}");
    assert!(last.contains("重命名后的会话"), "{last}");

    let info = V3SessionStorage::inspect(&path).unwrap();
    assert_eq!(info.name.as_deref(), Some("重命名后的会话"));
}

#[test]
fn repo_creates_lists_opens_and_deletes() {
    let root = std::env::temp_dir().join(format!("workman-repo-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = V3SessionRepo::new(&root);

    let created = repo
        .create_at("/tmp/demo-project", &Default::default())
        .unwrap();
    let id = created.metadata().id.clone();
    assert!(created.path().exists());

    let listed = repo.scan();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, id);
    assert_eq!(listed[0].cwd, "/tmp/demo-project");

    let reopened = repo.open_by_id(&id).unwrap();
    assert_eq!(reopened.metadata().id, id);

    repo.delete_by_id(&id).unwrap();
    assert!(repo.scan().is_empty());

    std::fs::remove_dir_all(&root).unwrap();
}

#[test]
fn repo_implements_session_repo_trait() {
    let root = std::env::temp_dir().join(format!("workman-repo-trait-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root).unwrap();
    let repo = V3SessionRepo::new(&root);

    let mut metadata = serde_json::Map::new();
    metadata.insert("cwd".into(), serde_json::json!("/tmp/demo-project"));
    let options = rpi_harness::session::types::SessionCreateOptions {
        id: None,
        parent_session_id: None,
        metadata: Some(metadata),
    };
    let storage = block_on(repo.create(&options)).unwrap();
    let meta = storage.metadata();

    let listed = block_on(repo.list()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, meta.id);

    let opened = block_on(repo.open(&meta)).unwrap();
    assert_eq!(opened.metadata().id, meta.id);

    block_on(repo.delete(&meta)).unwrap();
    std::fs::remove_dir_all(&root).unwrap();
}

/// 真实 pi 历史会话的回归：目录存在就全量加载一遍。
/// 没有 pi 数据的机器上自动跳过。
#[test]
fn loads_real_pi_sessions_if_present() {
    let Some(home) = std::env::var_os("HOME") else {
        return;
    };
    let root = PathBuf::from(home).join(".pi/agent/sessions");
    if !root.is_dir() {
        eprintln!("跳过：没有 {}", root.display());
        return;
    }
    let repo = V3SessionRepo::new(&root);
    let sessions = repo.scan();
    if sessions.is_empty() {
        return;
    }
    // 全量太大，取最近 12 个够了（含大文件与分支文件）
    let mut failures = Vec::new();
    for info in sessions.iter().take(12) {
        match V3SessionStorage::load(&info.path) {
            Ok(storage) => {
                let report = storage.load_report();
                if !report.skipped.is_empty() {
                    failures.push(format!(
                        "{}: {} 行被跳过，首条 {}",
                        info.path.display(),
                        report.skipped.len(),
                        report.skipped[0]
                    ));
                }
            }
            Err(e) => failures.push(format!("{}: {e}", info.path.display())),
        }
    }
    assert!(
        failures.is_empty(),
        "真实会话加载失败：\n{}",
        failures.join("\n")
    );
}
