//! v3 会话兼容性探针。
//!
//! pi 的 `.jsonl` 是 v3：
//!   {"type":"session","version":3,"id","timestamp":"ISO","cwd"}
//!   {"type":"message","id","parentId","timestamp":"ISO","message":{"role":...}}
//!
//! rpi 的 jsonl 层只认 v4（`{"kind":"header","version":4,"createdAt"}` +
//! 条目带 `seq`），`decode_header` 对 version != 4 直接报错。
//!
//! 这个探针不解码 v4，而是**绕过 rpi 的 jsonl 层**，直接用
//! `rpi_agent::AgentMessage` 反序列化 v3 的 message 载荷 —— 验证「自己写一个
//! SessionStorage 就能继续用 v3 文件」这个方案是否成立，并统计失败原因。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::Result;
use serde_json::Value;

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

/// v3 ISO 时间戳 → unix ms（rpi 的条目层用 i64 ms）。
fn iso_to_ms(iso: &str) -> Option<i64> {
    // 形如 2026-10-10T11:01:13.523Z
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
    let ms: i64 = format!("{:0<3}", &frac[..frac.len().min(3)]).parse().ok()?;

    // days since epoch (Howard Hinnant 的 days_from_civil)
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;

    Some(((days * 86400) + h * 3600 + mi * 60 + s) * 1000 + ms)
}

fn main() -> Result<()> {
    let root = sessions_root()?;
    let mut files = Vec::new();
    collect_jsonl(&root, &mut files)?;
    files.sort();
    println!("扫描 {} 个 v3 会话文件：{}\n", files.len(), root.display());

    let mut headers_ok = 0usize;
    let mut total_entries = 0usize;
    let mut entry_types: BTreeMap<String, usize> = BTreeMap::new();
    let mut msg_roles: BTreeMap<String, usize> = BTreeMap::new();
    let mut decode_ok = 0usize;
    let mut decode_fail: BTreeMap<String, usize> = BTreeMap::new();
    // 检查 v3 条目层字段能否完整映射到 rpi 的 EntryBase 需求
    let mut missing_seq = 0usize;
    let mut iso_ts_ok = 0usize;
    let mut tooluse_normalized = 0usize;

    for path in &files {
        let Ok(text) = std::fs::read_to_string(path) else {
            continue;
        };
        let mut lines = text.lines().filter(|l| !l.trim().is_empty());

        let Some(first) = lines.next() else { continue };
        let Ok(header) = serde_json::from_str::<Value>(first) else {
            continue;
        };
        if header.get("type").and_then(Value::as_str) != Some("session") {
            continue;
        }
        if header.get("version").and_then(Value::as_u64) != Some(3) {
            continue;
        }
        headers_ok += 1;

        for line in lines {
            let Ok(entry) = serde_json::from_str::<Value>(line) else {
                continue;
            };
            total_entries += 1;
            let entry_type = entry
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("<none>")
                .to_string();
            *entry_types.entry(entry_type.clone()).or_default() += 1;

            // 条目层：v3 有 timestamp(ISO)，没有 seq
            if entry.get("seq").is_none() {
                missing_seq += 1;
            }
            if entry
                .get("timestamp")
                .and_then(Value::as_str)
                .and_then(iso_to_ms)
                .is_some()
            {
                iso_ts_ok += 1;
            }

            if entry_type != "message" {
                continue;
            }
            let Some(message) = entry.get("message") else {
                continue;
            };
            let role = message
                .get("role")
                .and_then(Value::as_str)
                .unwrap_or("<none>")
                .to_string();
            *msg_roles.entry(role.clone()).or_default() += 1;

            // 核心验证：v3 的 message 载荷能否直接被 rpi 的类型吃下。
            // 注意 rpi 有两层：
            //   rpi_ai::Message        ← tag = "role"  = pi 的线格式 ✅
            //   rpi_agent::AgentMessage ← tag = "kind" = rpi 自己的持久化格式 ❌
            // 所以 v3 文件应该直接映射到 rpi_ai::Message，绕开 AgentMessage。
            // system 条目是 pi 的 transcript 内部产物（承载整份 prompt + 工具
            // schema），rpi 侧不需要持久化它，单独计数而不算失败。
            if role == "system" {
                continue;
            }
            let mut m = message.clone();
            // v3 把时间戳放在条目层，message 内层没有；rpi 的消息结构需要它。
            if m.get("timestamp").is_none() {
                let ts = entry
                    .get("timestamp")
                    .and_then(Value::as_str)
                    .and_then(iso_to_ms)
                    .unwrap_or(0);
                if let Some(obj) = m.as_object_mut() {
                    obj.insert("timestamp".into(), Value::from(ts));
                }
            }
            // 已发现的唯一真实差异：pi 写 `stopReason: "toolUse"`（camelCase），
            // 而 rpi_ai::StopReason 是 `rename_all = "lowercase"` → 期待 `"tooluse"`。
            // 适配层一行归一就能过，这里先就地验证。
            if let Some(obj) = m.as_object_mut() {
                if let Some(sr) = obj.get_mut("stopReason") {
                    if let Some(s) = sr.as_str() {
                        if s == "toolUse" {
                            *sr = Value::from("tooluse");
                            tooluse_normalized += 1;
                        }
                    }
                }
            }
            let decoded: Result<(), String> = match role.as_str() {
                "user" | "assistant" | "toolResult" => {
                    serde_json::from_value::<rpi_ai::Message>(m).map(|_| ()).map_err(|e| e.to_string())
                }
                // pi 的自定义消息（bashExecution 等）→ rpi 的 Custom 臂。
                // pi 把它存成 `{role, content:"..."}`（纯字符串），
                // rpi 的 CustomMessage 要求 `content: [blocks]`，同样要归一。
                _ => {
                    if let Some(obj) = m.as_object_mut() {
                        // bashExecution 连 content 都没有，只有 command/output/exitCode。
                        if obj.get("content").is_none() {
                            let text = obj
                                .remove("output")
                                .and_then(|v| v.as_str().map(str::to_string))
                                .unwrap_or_default();
                            obj.insert(
                                "content".into(),
                                Value::Array(vec![serde_json::json!({"type":"text","text":text})]),
                            );
                        }
                        if obj.get("content").is_some_and(Value::is_string) {
                            let s = obj.remove("content").and_then(|v| v.as_str().map(str::to_string)).unwrap_or_default();
                            obj.insert("content".into(), Value::Array(vec![serde_json::json!({"type":"text","text":s})]));
                        }
                        obj.entry("data").or_insert(Value::Null);
                        obj.entry("timestamp").or_insert(Value::from(0));
                    }
                    serde_json::from_value::<rpi_agent::CustomMessage>(m)
                        .map(|_| ())
                        .map_err(|e| e.to_string())
                }
            };
            match decoded {
                Ok(()) => decode_ok += 1,
                Err(e) => {
                    let key = format!("{role}: {}", short_reason(&e));
                    *decode_fail.entry(key).or_default() += 1;
                }
            }
        }
    }

    println!("v3 header 识别       : {headers_ok}/{}", files.len());
    println!("条目总数             : {total_entries}");
    println!("  缺 seq 的条目      : {missing_seq}（v3 无此字段，需 storage 层补）");
    println!("  时间戳可解析       : {iso_ts_ok}/{total_entries}");
    println!("  toolUse 归一       : {tooluse_normalized}（camelCase → lowercase）");
    println!("\n条目类型分布：");
    for (k, v) in &entry_types {
        println!("  {k:24} {v}");
    }
    println!("\nmessage 角色分布：");
    for (k, v) in &msg_roles {
        println!("  {k:24} {v}");
    }

    let attemptable: usize = msg_roles
        .iter()
        .filter(|(k, _)| k.as_str() != "system")
        .map(|(_, v)| *v)
        .sum();
    println!("\n★ rpi_ai::Message 反序列化 : {decode_ok}/{attemptable} 成功");
    if !decode_fail.is_empty() {
        println!("  失败原因：");
        let mut rows: Vec<_> = decode_fail.iter().collect();
        rows.sort_by_key(|(_, v)| std::cmp::Reverse(**v));
        for (k, v) in rows.iter().take(12) {
            println!("    {v:5}  {k}");
        }
    }
    Ok(())
}

/// serde 的错误信息很长，只留最关键的一句。
fn short_reason(s: &str) -> String {
    let s = s.split(" at line").next().unwrap_or(s);
    s.chars().take(110).collect()
}
