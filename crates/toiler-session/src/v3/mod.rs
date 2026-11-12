//! pi 的 v3 会话文件格式（`.jsonl`）。
//!
//! ```text
//! {"type":"session","version":3,"id","timestamp":"ISO","cwd"}   ← header，恰好一行
//! {"type":"message","id","parentId","timestamp":"ISO","message":{…}}
//! {"type":"model_change","id","parentId","timestamp","provider","modelId"}
//! {"type":"thinking_level_change","id","parentId","timestamp","thinkingLevel"}
//! {"type":"compaction","id","parentId","timestamp","summary","tokensBefore",…}
//! {"type":"session_info","id","parentId","timestamp","name"}     ← pi 私有
//! {"type":"context_edit","id","parentId","timestamp","targetId","replacement"}
//! ```
//!
//! rpi 的 jsonl 层只认 v4（`{"kind":"header","version":4}` + 条目带 `seq`），
//! 对它 `version != 4` 直接报错。所以我们不走 `JsonlSessionRepo`，
//! 而是在这一层自己编解码，再把内容喂给 rpi 的内存后端。

pub mod entry;
pub mod error;
pub mod header;
pub mod message;

pub use entry::{decode_entry, encode_entry, is_known_v3_entry_type};
pub use error::V3Error;
pub use header::V3Header;
pub use message::{decode_message, encode_message};

use serde_json::Value;

/// 从一行文本解析会话条目（header 除外）。
///
/// `line_no` 只用于报错；`seq` 由调用方给（v3 没有该字段，按成功落地顺序递增）。
pub fn parse_entry_line(
    line: &str,
    seq: u64,
    line_no: usize,
) -> Result<entry::ParsedEntry, V3Error> {
    let value: Value =
        serde_json::from_str(line).map_err(|e| V3Error::syntax(line_no, e.to_string()))?;
    let ty = value
        .get("type")
        .and_then(Value::as_str)
        .ok_or_else(|| V3Error::schema(line_no, "条目缺少 type"))?
        .to_string();
    let parent_id = value
        .get("parentId")
        .and_then(Value::as_str)
        .map(str::to_string);
    let entry = decode_entry(&value, seq)?;
    Ok(entry::ParsedEntry {
        entry_type: ty,
        parent_id,
        entry,
    })
}
