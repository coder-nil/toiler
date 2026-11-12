//! v3 会话文件的 header（第一行）。
//!
//! ```json
//! {"type":"session","version":3,"id":"01a1c430-…","timestamp":"2026-11-10T06:41:44.431Z","cwd":"/path"}
//! ```
//!
//! 与 rpi 的 v4 header（`{"kind":"header","version":4,"createdAt":…}`）不同：
//! v3 用 `type`/`timestamp`/`version:3`，这正是 pi 自己的写法。我们**照样写 v3**，
//! 这样 pi 仍然能打开我们新建的会话文件。

use serde_json::{json, Map, Value};

use super::error::V3Error;
use crate::time::{iso_to_ms, ms_to_iso};

/// v3 header 的反序列化结果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct V3Header {
    pub id: String,
    /// unix 毫秒（文件里写的是 ISO 字符串）。
    pub created_at: i64,
    pub cwd: String,
}

impl V3Header {
    pub fn new(id: impl Into<String>, created_at: i64, cwd: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            created_at,
            cwd: cwd.into(),
        }
    }

    /// 解析第一行。
    pub fn parse(line: &str) -> Result<Self, V3Error> {
        let v: Value = serde_json::from_str(line).map_err(|e| V3Error::syntax(1, e.to_string()))?;
        let obj = v
            .as_object()
            .ok_or_else(|| V3Error::schema(1, "header 不是 JSON 对象"))?;

        if obj.get("type").and_then(Value::as_str) != Some("session") {
            return Err(V3Error::schema(1, "header 的 type 不是 \"session\""));
        }
        match obj.get("version").and_then(Value::as_u64) {
            Some(3) => {}
            Some(other) => {
                return Err(V3Error::schema(
                    1,
                    format!("不是 v3 会话（version={other}）"),
                ))
            }
            None => return Err(V3Error::schema(1, "header 缺少 version")),
        }

        let id = obj
            .get("id")
            .and_then(Value::as_str)
            .ok_or_else(|| V3Error::schema(1, "header 缺少 id"))?
            .to_string();
        let cwd = obj
            .get("cwd")
            .and_then(Value::as_str)
            .ok_or_else(|| V3Error::schema(1, "header 缺少 cwd"))?
            .to_string();
        let created_at = match obj.get("timestamp") {
            Some(Value::String(iso)) => iso_to_ms(iso).ok_or_else(|| V3Error::Timestamp {
                line: 1,
                value: iso.clone(),
            })?,
            Some(Value::Number(n)) => n
                .as_i64()
                .ok_or_else(|| V3Error::schema(1, "header.timestamp 不是整数"))?,
            _ => return Err(V3Error::schema(1, "header 缺少 timestamp")),
        };

        Ok(Self {
            id,
            created_at,
            cwd,
        })
    }

    /// 序列化成 v3 第一行（含结尾换行）。
    pub fn to_line(&self) -> String {
        let value = json!({
            "type": "session",
            "version": 3,
            "id": self.id,
            "timestamp": ms_to_iso(self.created_at),
            "cwd": self.cwd,
        });
        // 键序固定，方便测试与肉眼 diff
        let mut line = String::new();
        line.push_str(&ordered_json(&value));
        line.push('\n');
        line
    }
}

/// 按键序写回：pi 的 header 顺序是 type/version/id/timestamp/cwd。
fn ordered_json(value: &Value) -> String {
    let Some(obj) = value.as_object() else {
        return value.to_string();
    };
    let order = ["type", "version", "id", "timestamp", "cwd"];
    let mut out = Map::new();
    for key in order {
        if let Some(v) = obj.get(key) {
            out.insert(key.to_string(), v.clone());
        }
    }
    for (k, v) in obj {
        if !out.contains_key(k) {
            out.insert(k.clone(), v.clone());
        }
    }
    Value::Object(out).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    const REAL: &str = r#"{"type":"session","version":3,"id":"01a1c430-f1ef-7516-b8ed-33ac88cc72cc","timestamp":"2026-11-10T06:41:44.431Z","cwd":"/Volumes/Chenxiaoni·SSD/Development/RustProjects/toiler"}"#;

    #[test]
    fn parses_real_pi_header() {
        let h = V3Header::parse(REAL).expect("parse");
        assert_eq!(h.id, "01a1c430-f1ef-7516-b8ed-33ac88cc72cc");
        assert_eq!(
            h.cwd,
            "/Volumes/Chenxiaoni·SSD/Development/RustProjects/toiler"
        );
        assert_eq!(h.created_at, iso_to_ms("2026-11-10T06:41:44.431Z").unwrap());
    }

    #[test]
    fn roundtrip_header_line() {
        let h = V3Header::parse(REAL).unwrap();
        assert_eq!(h.to_line().trim_end(), REAL);
        let again = V3Header::parse(h.to_line().trim_end()).unwrap();
        assert_eq!(h, again);
    }

    #[test]
    fn rejects_v4_and_junk() {
        assert!(V3Header::parse(r#"{"kind":"header","version":4}"#).is_err());
        assert!(V3Header::parse("not json").is_err());
        assert!(V3Header::parse(r#"{"type":"session","version":3}"#).is_err());
    }
}
