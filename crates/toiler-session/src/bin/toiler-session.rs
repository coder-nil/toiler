//! `toiler-session` 的命令行入口。
//!
//! ```bash
//! toiler-session init                     # 初始化 ~/.toiler/agent/sessions
//! toiler-session list [root]              # 列会话（默认 ~/.toiler，也可指向 ~/.pi）
//! toiler-session show <file>              # 看一个会话文件：header / 条目 / 名字
//! toiler-session load <file>              # 回放进 rpi 后端，报告跳过的行
//! toiler-session append <file> "文本"      # 加载后追加一轮 user+assistant 消息
//! ```

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use rpi_agent::message::{AgentMessage, CustomMessage};
use rpi_harness::session::types::{EntryQuery, SessionStorage};
use rpi_harness::session::Session;
use toiler_session::{ToilerHome, V3SessionRepo, V3SessionStorage};

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let cmd = args.next().unwrap_or_else(|| "help".to_string());
    let rest: Vec<String> = args.collect();

    match cmd.as_str() {
        "init" => init(),
        "verify" => verify(rest.first().map(String::as_str)),
        "list" => list(rest.first().map(String::as_str)),
        "show" => show(rest.first().map(String::as_str)),
        "load" => load(rest.first().map(String::as_str)),
        "append" => append(
            rest.first().map(String::as_str),
            rest.get(1).map(String::as_str).unwrap_or("（空消息）"),
        ),
        "help" | "-h" | "--help" => {
            println!("用法： toiler-session <init|list|show|load|append|verify> [参数]");
            Ok(())
        }
        other => bail!("未知子命令 {other}"),
    }
}

fn home() -> Result<ToilerHome> {
    ToilerHome::discover()
}

fn init() -> Result<()> {
    let home = home()?;
    home.ensure()?;
    println!("✔ 已初始化 {}", home.sessions_root().display());
    println!("  （与 pi 的 ~/.pi/agent/sessions 同构，可直接复制会话文件）");
    Ok(())
}

fn list(root: Option<&str>) -> Result<()> {
    let root = match root {
        Some(r) => PathBuf::from(r),
        None => home()?.sessions_root(),
    };
    let repo = V3SessionRepo::new(&root);
    let sessions = repo.scan();
    println!("{} 个会话（{}）\n", sessions.len(), root.display());
    for info in sessions.iter().take(30) {
        let name = info
            .name
            .clone()
            .or_else(|| repo.inspect(&info.id).and_then(|i| i.name))
            .unwrap_or_else(|| "-".into());
        println!(
            "  {}  {:>6} 条  {}\n      {}\n      {}",
            toiler_session::ms_to_iso(info.created_at),
            info.entries,
            name,
            info.cwd,
            info.path.display()
        );
    }
    Ok(())
}

/// 全量回归：把一个 sessions 根下的每个会话都回放一遍，汇总跳过的行。
fn verify(root: Option<&str>) -> Result<()> {
    let root = match root {
        Some(r) => PathBuf::from(r),
        None => home()?.sessions_root(),
    };
    let repo = V3SessionRepo::new(&root);
    let sessions = repo.scan();
    if sessions.is_empty() {
        println!("{} 下没有会话", root.display());
        return Ok(());
    }

    let mut ok = 0usize;
    let mut entries = 0usize;
    let mut failures: Vec<String> = Vec::new();
    for info in &sessions {
        match V3SessionStorage::load(&info.path) {
            Ok(storage) => {
                let report = storage.load_report();
                entries += report.loaded_entries;
                if report.skipped.is_empty() {
                    ok += 1;
                } else {
                    failures.push(format!(
                        "{}：跳过 {} 行（首条 {}）",
                        info.path.display(),
                        report.skipped.len(),
                        report.skipped[0]
                    ));
                }
            }
            Err(e) => failures.push(format!("{}：{e}", info.path.display())),
        }
    }

    println!("会话文件      : {}", sessions.len());
    println!("★ 完整加载    : {ok}/{}", sessions.len());
    println!("回放条目      : {entries}");
    if !failures.is_empty() {
        println!("\n有问题的文件：");
        for f in failures.iter().take(20) {
            println!("  - {f}");
        }
    }
    Ok(())
}

fn show(path: Option<&str>) -> Result<()> {
    let path = path.context("需要会话文件路径")?;
    let info = V3SessionStorage::inspect(path)?;
    println!("id        : {}", info.id);
    println!("created_at: {}", toiler_session::ms_to_iso(info.created_at));
    println!("cwd       : {}", info.cwd);
    println!("entries   : {}", info.entries);
    println!("name      : {}", info.name.unwrap_or_else(|| "-".into()));
    println!("path      : {}", info.path.display());
    Ok(())
}

fn load(path: Option<&str>) -> Result<()> {
    let path = path.context("需要会话文件路径")?;
    let storage = V3SessionStorage::load(path)?;
    let report = storage.load_report();
    let stats = block_on(storage.get_stats())?;
    println!("✔ 回放成功 {}", path);
    println!("  条目（已回放）  : {}", report.loaded_entries);
    println!("  跳过的行        : {}", report.skipped.len());
    for s in report.skipped.iter().take(10) {
        println!("    - {s}");
    }
    println!("  消息数          : {}", stats.message_count);
    let name = block_on(storage.get_name())?;
    println!("  会话名          : {}", name.unwrap_or_else(|| "-".into()));
    println!("  文件            : {}", storage.path().display());
    Ok(())
}

fn append(path: Option<&str>, text: &str) -> Result<()> {
    let path = path.context("需要会话文件路径")?;
    let storage = V3SessionStorage::load(path)?;
    let before = block_on(storage.get_stats())?.message_count;
    let session = Session::new(Arc::new(storage), None);

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0);

    let user = rpi_ai::types::UserMessage::new(text, now);
    let user_id = block_on(session.append_message(AgentMessage::User(user)))?;

    let reply = format!("（toiler 写入的占位回复）收到：{text}");
    // 用 pi 官方认可的自定义消息形状：role = "custom" + customType
    let assistant = CustomMessage::new(
        "custom",
        vec![rpi_ai::types::Content::text(&reply)],
        serde_json::json!({ "customType": "toiler", "display": true }),
        now + 1,
    );
    let assistant_id = block_on(session.append_message(AgentMessage::Custom(assistant)))?;

    let after = block_on(session.get_stats())?.message_count;
    println!("✔ 已追加一轮：user={user_id} assistant={assistant_id}");
    println!("  message_count {before} → {after}");
    println!("  文件行数现在：{}", count_lines(path)?);
    Ok(())
}

fn count_lines(path: &str) -> Result<usize> {
    let text = std::fs::read_to_string(path)?;
    Ok(text.lines().filter(|l| !l.trim().is_empty()).count())
}

/// 最小的同步执行器：CLI 里不值得为一个 main 引入 tokio 依赖。
fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    tokio_lite::block_on(fut)
}

mod tokio_lite {
    use std::future::Future;
    use std::pin::pin;
    use std::task::{Context, Poll, RawWaker, RawWakerVTable, Waker};

    fn noop_waker() -> Waker {
        fn clone(_: *const ()) -> RawWaker {
            RawWaker::new(std::ptr::null(), &VTABLE)
        }
        fn noop(_: *const ()) {}
        static VTABLE: RawWakerVTable = RawWakerVTable::new(clone, noop, noop, noop);
        unsafe { Waker::from_raw(RawWaker::new(std::ptr::null(), &VTABLE)) }
    }

    /// 只驱动「立即就绪」的 future（我们的存储层是同步的，不需要真 reactor）。
    pub fn block_on<F: Future>(fut: F) -> F::Output {
        let waker = noop_waker();
        let mut cx = Context::from_waker(&waker);
        let mut fut = pin!(fut);
        loop {
            if let Poll::Ready(out) = fut.as_mut().poll(&mut cx) {
                return out;
            }
        }
    }
}

/// 让 `EntryQuery::default()` 在 CLI 里少写几个字。
#[allow(dead_code)]
fn all_entries() -> EntryQuery {
    EntryQuery::default()
}
