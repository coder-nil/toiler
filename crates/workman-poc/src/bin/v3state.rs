//! v3 → `SessionState` 回放探针。
//!
//! 目的：在写正式实现之前，先确认真实 v3 文件能不能**逐行**喂进
//! `rpi_harness::session::SessionState::apply_mutation`（严格校验 seq 连续、
//! id 唯一、lane 叶子链）。任何一条不满足，整个会话就加载不出来，所以必须
//! 先量化失败率与失败原因。
//!
//! 同时验证反方向：`Entry::to_flat_json()` 能不能还原成 v3 行（用于写穿）。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use rpi_harness::session::types::{Entry, SessionMutation};
use rpi_harness::session::SessionState;
use serde_json::{json, Map, Value};

fn sessions_root() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").ok_or_else(|| anyhow::anyhow!("HOME"))?;
    Ok(PathBuf::from(home).join(".pi/agent/sessions"))
}

fn collect_jsonl(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_jsonl(&path, out)?;
        } else if path.extension().is_some_and(|e| e == "jsonl") {
            out.push(path);
        }
    }
    Ok(())
}

// ---------------------------------------------------------------- timestamps

/// v3 ISO 时间戳（`2026-10-10T11:01:13.523Z`）→ unix ms。
pub fn iso_to_ms(iso: &str) -> Option<i64> {
    let (date, rest) = iso.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    let time = rest.trim_end_matches('Z');
    let (hms, frac) = match time.split_once('.') {
        Some((a, b)) => (a, b),
        None => (time, "0"),
    };
    let mut t = hms.split(':');
    let h: i64 = t.next()?.parse().ok()?;
    let mi: i64 = t.next()?.parse().ok()?;
    let s: i64 = t.next()?.parse().ok()?;
    let frac = &frac[..frac.len().min(3)];
    let ms: i64 = format!("{frac:0<3}").parse().ok()?;

    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;
    Some(((days * 86400) + h * 3600 + mi * 60 + s) * 1000 + ms)
}

// ------------------------------------------------------------ entry 归一化

/// 把一行 v3 条目归一成 rpi `Entry` 能直接反序列化的 JSON。
fn normalize_entry(v: &Value, seq: u64) -> Result<Value, String> {
    let obj = v.as_object().ok_or("entry is not an object")?;
    let ty = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or("entry has no type")?;

    let mut out = Map::new();
    out.insert("seq".into(), json!(seq));
    for (k, val) in obj {
        if k == "type" || k == "seq" {
            continue;
        }
        out.insert(k.clone(), val.clone());
    }
    // v3 的 timestamp 是 ISO 字符串，rpi 条目层要 i64 ms
    if let Some(Value::String(iso)) = obj.get("timestamp") {
        let ms = iso_to_ms(iso).ok_or_else(|| format!("bad timestamp {iso}"))?;
        out.insert("timestamp".into(), json!(ms));
    } else if obj.get("timestamp").is_none() {
        out.insert("timestamp".into(), json!(0));
    }
    if !obj.contains_key("parentId") {
        out.insert("parentId".into(), Value::Null);
    }

    match ty {
        "message" => {
            let msg = obj.get("message").cloned().ok_or("message entry has no message")?;
            let ts = out.get("timestamp").and_then(Value::as_i64).unwrap_or(0);
            let agent = v3_message_to_agent(&msg, ts)?;
            out.insert("message".into(), agent);
            out.insert("type".into(), json!("message"));
        }
        "model_change" => {
            out.insert("type".into(), json!("model_change"));
        }
        "thinking_level_change" => {
            out.insert("type".into(), json!("thinking_level_change"));
        }
        "compaction" => {
            out.insert("type".into(), json!("compaction"));
            // v3 专有字段存进 details.__v3，写回时原样摊平
            let mut stash = Map::new();
            for k in ["firstKeptEntryId", "fromHook", "systemMessage"] {
                if let Some(v) = obj.get(k) {
                    stash.insert(k.to_string(), v.clone());
                }
            }
            let mut details = obj
                .get("details")
                .and_then(Value::as_object)
                .cloned()
                .unwrap_or_default();
            if !stash.is_empty() {
                details.insert("__v3".into(), Value::Object(stash));
            }
            if !details.is_empty() {
                out.insert("details".into(), Value::Object(details));
            }
            for k in ["firstKeptEntryId", "fromHook", "systemMessage"] {
                out.remove(k);
            }
            // rpi 要 retainedTail（[] 合法）；v3 用 firstKeptEntryId 表达保留位置
            out.entry("retainedTail")
                .or_insert_with(|| Value::Array(vec![]));
            out.entry("tokensBefore").or_insert(json!(0));
            // firstKeptEntryId / fromHook / systemMessage 留在 details 里
        }
        "branch_summary" => {
            out.insert("type".into(), json!("branch_summary"));
        }
        "active_tools_change" => {
            out.insert("type".into(), json!("active_tools_change"));
        }
        "custom" => {
            out.insert("type".into(), json!("custom"));
        }
        // session_info / context_edit / 其它 pi 私有条目 → rpi 的 Custom 条目
        other => {
            out.insert("type".into(), json!("custom"));
            out.insert("customType".into(), json!(other));
            let mut data = obj.clone();
            data.remove("type");
            data.remove("id");
            data.remove("parentId");
            data.remove("timestamp");
            data.remove("seq");
            out.insert("data".into(), Value::Object(data));
        }
    }
    Ok(Value::Object(out))
}

/// v3 消息（pi 线格式，`role` 判别）→ `rpi_agent::AgentMessage`（`kind` 判别）。
fn v3_message_to_agent(m: &Value, ts: i64) -> Result<Value, String> {
    let obj = m.as_object().ok_or("message is not an object")?;
    let role = obj
        .get("role")
        .and_then(Value::as_str)
        .ok_or("message has no role")?;

    let mut msg = obj.clone();
    msg.entry("timestamp").or_insert(json!(ts));

    match role {
        "user" => {
            msg.insert("kind".into(), json!("user"));
            Ok(Value::Object(msg))
        }
        "assistant" => {
            msg.insert("kind".into(), json!("assistant"));
            // v3 写 "toolUse"，rpi 的 StopReason 只认小写 "tooluse"
            if let Some(Value::String(s)) = msg.get("stopReason").cloned() {
                msg.insert("stopReason".into(), json!(s.to_ascii_lowercase()));
            }
            Ok(Value::Object(msg))
        }
        "toolResult" => {
            msg.insert("kind".into(), json!("tool_result"));
            // v3 camelCase 的 toolCallId/toolName/isError 与 rpi camelCase 一致
            Ok(Value::Object(msg))
        }
        // system / bashExecution / 其它 → CustomMessage
        other => {
            let mut data = obj.clone();
            data.remove("role");
            data.remove("content");
            data.remove("timestamp");
            let content = match obj.get("content") {
                Some(Value::String(s)) if other == "bashExecution" => {
                    // v3 的 bashExecution 没有 content，用 output 合成文本块
                    let output = obj
                        .get("output")
                        .and_then(Value::as_str)
                        .unwrap_or_default();
                    json!([{ "type": "text", "text": output }])
                }
                // 空字符串保持空字符串（v3 的 system 消息就这样写）
                Some(Value::String(s)) if s.is_empty() => json!(""),
                Some(Value::String(s)) => json!([{ "type": "text", "text": s }]),
                Some(Value::Array(a)) => Value::Array(a.clone()),
                _ => json!([]),
            };
            let inner_ts = obj
                .get("timestamp")
                .and_then(Value::as_i64)
                .unwrap_or(ts);
            let custom = json!({
                "kind": "custom",
                "role": other,
                "content": content,
                "data": Value::Object(data),
                "timestamp": inner_ts,
            });
            Ok(custom)
        }
    }
}

// ---------------------------------------------------------------- 回放

struct Report {
    files: usize,
    loaded: usize,
    entries: usize,
    failures: BTreeMap<String, usize>,
    roundtrip_ok: usize,
    roundtrip_diff: BTreeMap<String, usize>,
    diff_samples: Vec<(String, String)>,
}

fn load_file(path: &Path, report: &mut Report) -> Result<()> {
    let text = std::fs::read_to_string(path)?;
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(first) = lines.next() else {
        return Ok(());
    };
    let header: Value = serde_json::from_str(first)?;
    if header.get("type").and_then(Value::as_str) != Some("session") {
        return Ok(());
    }
    if header.get("version").and_then(Value::as_u64) != Some(3) {
        return Ok(());
    }
    report.files += 1;

    let mut state = SessionState::new();
    let mut local_fail: Option<String> = None;

    for line in lines {
        let Ok(v) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        // seq 只在成功落地后推进，避免一条失败引发的整串「非连续」级联
        report.entries += 1;
        let entry_type = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or("<none>")
            .to_string();

        let seq = state.next_sequence();
        let normalized = match normalize_entry(&v, seq) {
            Ok(n) => n,
            Err(e) => {
                *report
                    .failures
                    .entry(format!("{entry_type}: normalize: {e}"))
                    .or_default() += 1;
                local_fail.get_or_insert(format!("{entry_type}: normalize: {e}"));
                continue;
            }
        };
        let entry: Entry = match serde_json::from_value(normalized) {
            Ok(e) => e,
            Err(e) => {
                let key = format!("{entry_type}: decode: {}", short(&e.to_string()));
                *report.failures.entry(key.clone()).or_default() += 1;
                local_fail.get_or_insert(key);
                continue;
            }
        };

        // 反向：Entry → v3 行
        match entry_to_v3_line(&entry) {
            Some(v3) => {
                report.roundtrip_ok += 1;
                let a = normalize_numbers(strip_seq(&v));
                let b = normalize_numbers(strip_seq(&v3));
                if a != b {
                    let key = format!("{entry_type}: diff");
                    *report.roundtrip_diff.entry(key.clone()).or_default() += 1;
                    if !report.diff_samples.iter().any(|(k, _)| *k == entry_type) {
                        report.diff_samples.push((entry_type.clone(), structural_diff(&a, &b, "")));
                    }
                }
            }
            None => {
                *report
                    .roundtrip_diff
                    .entry(format!("{entry_type}: no writer"))
                    .or_default() += 1;
            }
        }

        // v3 是一个 parentId 链，可能分支（rewind / edit）。rpi 的 lane 叶子链
        // 要求 parentId == 当前叶子，所以分支时先把 main 的叶子退回到 parent。
        let parent = entry.parent_id().map(str::to_string);
        let leaf = state
            .get_lanes()
            .into_iter()
            .find(|l| l.lane == "main")
            .and_then(|l| l.leaf_id.clone());
        let mut lane = Some("main".to_string());
        if parent != leaf {
            match parent.as_deref() {
                Some(p) if state.get_entry(p).is_some() => {
                    let s = state.next_sequence();
                    if let Err(e) = state.apply_mutation(SessionMutation::Lane {
                        seq: s,
                        lane: "main".to_string(),
                        leaf_id: Some(p.to_string()),
                    }) {
                        let key = format!("{entry_type}: rewind: {}", short(&e.to_string()));
                        *report.failures.entry(key.clone()).or_default() += 1;
                        local_fail.get_or_insert(key);
                    }
                }
                // 孤立的根（父已被删）或父缺失：按 fork 路径不校验链
                _ => lane = None,
            }
        }

        let seq = state.next_sequence();
        let mutation = SessionMutation::Entry {
            seq,
            timestamp: entry.base().timestamp,
            lane,
            entry,
        };
        if let Err(e) = state.apply_mutation(mutation) {
            let key = format!("{entry_type}: apply: {}", short(&e.to_string()));
            *report.failures.entry(key.clone()).or_default() += 1;
            local_fail.get_or_insert(key);
        }
    }

    if local_fail.is_none() {
        report.loaded += 1;
    }
    Ok(())
}

/// `Entry` → v3 行（`seq` 丢掉、`timestamp` 转回 ISO、`custom` 摊回原条目类型）。
fn entry_to_v3_line(entry: &Entry) -> Option<Value> {
    let mut flat = entry.to_flat_json();
    let obj = flat.as_object_mut()?;
    obj.remove("seq");
    if let Some(ts) = obj.get("timestamp").and_then(Value::as_i64) {
        obj.insert("timestamp".into(), json!(ms_to_iso(ts)));
    }
    if entry.entry_type() == "custom" {
        let custom_type = obj
            .remove("customType")
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_else(|| "custom".into());
        if let Some(Value::Object(data)) = obj.remove("data") {
            for (k, v) in data {
                obj.insert(k, v);
            }
        }
        obj.insert("type".into(), json!(custom_type));
    }
    // message 条目：AgentMessage 的 kind 判别去掉，role 保留（v3 线格式）
    if entry.entry_type() == "message" {
        if let Some(Value::Object(msg)) = obj.get_mut("message") {
            let is_custom = msg.get("kind").and_then(Value::as_str) == Some("custom");
            msg.remove("kind");
            if is_custom {
                // CustomMessage {role, content, data, timestamp} → v3 的扁平 role + 载荷
                let role = msg.remove("role").unwrap_or(json!("custom"));
                let content = msg.remove("content");
                let ts = msg.remove("timestamp");
                if let Some(Value::Object(data)) = msg.remove("data") {
                    for (k, v) in data {
                        msg.insert(k, v);
                    }
                }
                msg.insert("role".into(), role);
                let keep_content = match &content {
                    Some(Value::String(s)) => !s.is_empty(),
                    Some(Value::Array(a)) => !a.is_empty(),
                    _ => false,
                };
                if keep_content {
                    if let Some(c) = content {
                        msg.insert("content".into(), c);
                    }
                }
                if let Some(t) = ts {
                    msg.insert("timestamp".into(), t);
                }
            }
            // 写回时恢复 pi 的 camelCase：tooluse → toolUse
            if let Some(Value::String(r)) = msg.get("stopReason").cloned() {
                let fixed = match r.as_str() {
                    "tooluse" => "toolUse",
                    "pending" => "pending",
                    other => other,
                };
                msg.insert("stopReason".into(), json!(fixed));
            }
        }
    }
    // compaction 条目：retainedTail 是 rpi 专有，v3 用 firstKeptEntryId
    if entry.entry_type() == "compaction" {
        obj.remove("retainedTail");
        let stash = obj
            .get_mut("details")
            .and_then(Value::as_object_mut)
            .and_then(|d| d.remove("__v3"))
            .and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            });
        let details_empty = obj
            .get("details")
            .and_then(Value::as_object)
            .is_some_and(|d| d.is_empty());
        if details_empty {
            obj.remove("details");
        }
        if let Some(stash) = stash {
            for (k, v) in stash {
                obj.insert(k, v);
            }
        }
    }
    Some(flat)
}

fn strip_seq(v: &Value) -> Value {
    let mut out = v.clone();
    if let Some(o) = out.as_object_mut() {
        o.remove("seq");
    }
    out
}

/// 比较时把数字统一成 f64 字符串（0 与 0.0 语义相同，但 serde_json 认为不等）。
fn normalize_numbers(v: Value) -> Value {
    match v {
        Value::Number(n) => Value::String(format!("{:?}", n.as_f64().unwrap_or(0.0))),
        Value::Array(a) => Value::Array(a.into_iter().map(normalize_numbers).collect()),
        Value::Object(o) => Value::Object(
            o.into_iter()
                .map(|(k, v)| (k, normalize_numbers(v)))
                .collect(),
        ),
        other => other,
    }
}

/// unix ms → `YYYY-MM-DDTHH:MM:SS.mmmZ`
pub fn ms_to_iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    // civil_from_days (Howard Hinnant)
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}T{h:02}:{mi:02}:{s:02}.{millis:03}Z")
}

/// 逐路径比较两个 JSON，输出差异路径（值超过 200 字符就截断）。
fn structural_diff(a: &Value, b: &Value, _path: &str) -> String {
    let mut out = String::new();
    fn walk(a: &Value, b: &Value, path: &str, out: &mut String) {
        match (a, b) {
            (Value::Object(x), Value::Object(y)) => {
                for k in x.keys() {
                    let p = format!("{path}.{k}");
                    match y.get(k) {
                        Some(v) => walk(&x[k], v, &p, out),
                        None => out.push_str(&format!("  -{p} (缺失)\n")),
                    }
                }
                for k in y.keys() {
                    if !x.contains_key(k) {
                        out.push_str(&format!("  +{path}.{k} = {}\n", trunc(&y[k])));
                    }
                }
            }
            (Value::Array(x), Value::Array(y)) => {
                if x.len() != y.len() {
                    out.push_str(&format!("  ~{path} 长度 {} vs {}\n", x.len(), y.len()));
                }
                for (i, (xi, yi)) in x.iter().zip(y.iter()).enumerate() {
                    walk(xi, yi, &format!("{path}[{i}]"), out);
                }
            }
            _ => {
                if a != b {
                    out.push_str(&format!("  ~{path} {} → {}\n", trunc(a), trunc(b)));
                }
            }
        }
    }
    walk(a, b, "", &mut out);
    if out.is_empty() {
        "  （无数值差异，仅键序不同）\n".into()
    } else {
        out.chars().take(3000).collect()
    }
}

fn trunc(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 120 {
        format!("{}…", s.chars().take(120).collect::<String>())
    } else {
        s
    }
}

fn short(s: &str) -> String {
    s.chars().take(120).collect()
}

fn main() -> Result<()> {
    let root = sessions_root()?;
    let mut files = Vec::new();
    collect_jsonl(&root, &mut files)?;
    files.sort();
    println!("扫描 {} 个文件：{}\n", files.len(), root.display());

    let mut report = Report {
        files: 0,
        loaded: 0,
        entries: 0,
        failures: BTreeMap::new(),
        roundtrip_ok: 0,
        roundtrip_diff: BTreeMap::new(),
        diff_samples: Vec::new(),
    };

    for path in &files {
        if let Err(e) = load_file(path, &mut report) {
            eprintln!("{}: {e}", path.display());
        }
    }

    println!("v3 会话文件        : {}", report.files);
    println!("★ 完整加载成功     : {}/{}", report.loaded, report.files);
    println!("条目总数           : {}", report.entries);
    println!("\n回放失败（按原因）：");
    let mut rows: Vec<_> = report.failures.iter().collect();
    rows.sort_by_key(|(_, v)| std::cmp::Reverse(**v));
    for (k, v) in rows.iter().take(20) {
        println!("  {v:6}  {k}");
    }
    if rows.is_empty() {
        println!("  （无）");
    }

    println!("\n★ 回写 v3 行成功    : {}", report.roundtrip_ok);
    let mut rows: Vec<_> = report.roundtrip_diff.iter().collect();
    rows.sort_by_key(|(_, v)| std::cmp::Reverse(**v));
    for (k, v) in rows.iter().take(20) {
        println!("  {v:6}  {k}");
    }
    for (t, s) in &report.diff_samples {
        println!("\n差异样本 [{t}]：\n{s}");
    }
    Ok(())
}
