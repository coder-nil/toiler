//! `system_time`：把本机时钟暴露成只读工具。
//!
//! 对应 pi-desktop 的 `lib/system-time-tool.ts`。输出字段逐字一致：
//! `date` / `time` / `weekday` / `timeZone` / `utcOffset` / `iso8601`。
//!
//! # 与 pi 的差异
//!
//! | 项 | pi（JS） | 这里 |
//! |---|---|---|
//! | 本地时间 | `Intl.DateTimeFormat` | `libc::localtime_r`（同样的夏令时规则） |
//! | IANA 时区名 | `Intl.DateTimeFormat().resolvedOptions().timeZone` | 读 `/etc/localtime` 的 zoneinfo 链接；拿不到就退回 `tm_zone` 缩写 |
//!
//! 为什么不用 `chrono` / `chrono-tz`：只需要「本地时间 + 偏移 + 时区名」三件事，
//! libc 都已经提供了；再引入一个时区数据库会让二进制多几 MB。

use std::sync::Arc;

use async_trait::async_trait;
use rpi_agent::agent_tool::AgentTool;
use rpi_agent::error::AgentError;
use rpi_agent::types::{AgentToolResult, TextContentOrImage, ToolResultPartial};
use rpi_ai::types::{Schema, Tool};
use serde_json::json;
use tokio_util::sync::CancellationToken;

/// 星期名用英文全称（pi 的 `Intl` 里是 `weekday: "long"` + `en-US`）。
const WEEKDAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

pub struct SystemTimeTool {
    schema: Tool,
}

pub fn create_system_time_tool() -> Arc<dyn AgentTool> {
    Arc::new(SystemTimeTool {
        schema: Tool {
            name: "system_time".to_string(),
            description: "Get the current date and time from the toiler host system. \
                          Use this before answering questions involving today, tomorrow, \
                          yesterday, relative dates, weekdays, deadlines, or the current time."
                .to_string(),
            parameters: Schema::new(json!({
                "type": "object",
                "properties": {},
                "additionalProperties": false,
            })),
            constrained_sampling: None,
        },
    })
}

#[async_trait]
impl AgentTool for SystemTimeTool {
    fn schema(&self) -> &Tool {
        &self.schema
    }

    fn label(&self) -> &str {
        "System Time"
    }

    async fn execute(
        &self,
        _tool_call_id: &str,
        _params: serde_json::Value,
        _signal: CancellationToken,
        _on_update: Arc<dyn Fn(ToolResultPartial) + Send + Sync>,
    ) -> Result<AgentToolResult, AgentError> {
        let now = std::time::SystemTime::now();
        let text = serde_json::to_string(&snapshot(now)).unwrap_or_default();
        Ok(AgentToolResult {
            content: vec![TextContentOrImage::text(text)],
            details: json!({}),
            ..Default::default()
        })
    }
}

/// 一次采样的结果。字段名与 pi 的 `SystemTime` 类型完全一致。
fn snapshot(now: std::time::SystemTime) -> serde_json::Value {
    let local = local_time(now);
    json!({
        "date": format!("{:04}-{:02}-{:02}", local.year, local.month, local.day),
        "time": format!("{:02}:{:02}:{:02}", local.hour, local.minute, local.second),
        "weekday": WEEKDAYS[local.weekday],
        "timeZone": time_zone_name(local.zone_abbrev.as_deref()),
        "utcOffset": format_offset(local.offset_seconds),
        "iso8601": iso8601_utc(now),
    })
}

struct LocalTime {
    year: i32,
    month: u32,
    day: u32,
    hour: u32,
    minute: u32,
    second: u32,
    /// 0 = 周日，与 `libc::tm.tm_wday` 一致。
    weekday: usize,
    offset_seconds: i64,
    zone_abbrev: Option<String>,
}

fn local_time(now: std::time::SystemTime) -> LocalTime {
    let secs = now
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as libc::time_t)
        .unwrap_or(0);
    // SAFETY: `localtime_r` 只写我们给的 tm，且 tm 已零初始化。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    let ok = unsafe { libc::localtime_r(&secs, &mut tm) };
    if ok.is_null() {
        // 极端情况下（例如 TZ 指向坏文件）退回 UTC，至少不要 panic。
        let utc = utc_parts(secs);
        return LocalTime {
            offset_seconds: 0,
            zone_abbrev: None,
            ..utc
        };
    }
    let zone_abbrev = {
        // SAFETY: 成功返回的 tm_zone 指向静态字符串或 NULL。
        let ptr = tm.tm_zone;
        if ptr.is_null() {
            None
        } else {
            Some(
                unsafe { std::ffi::CStr::from_ptr(ptr) }
                    .to_string_lossy()
                    .to_string(),
            )
        }
    };
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
        weekday: (tm.tm_wday.clamp(0, 6)) as usize,
        offset_seconds: tm.tm_gmtoff as i64,
        zone_abbrev,
    }
}

fn utc_parts(secs: libc::time_t) -> LocalTime {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: 同上，gmtime_r 只写 tm。
    let ok = unsafe { libc::gmtime_r(&secs, &mut tm) };
    if ok.is_null() {
        return LocalTime {
            year: 1970,
            month: 1,
            day: 1,
            hour: 0,
            minute: 0,
            second: 0,
            weekday: 4,
            offset_seconds: 0,
            zone_abbrev: None,
        };
    }
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
        weekday: (tm.tm_wday.clamp(0, 6)) as usize,
        offset_seconds: 0,
        zone_abbrev: None,
    }
}

/// `+08:00` / `-05:30`（pi 用 `Intl` 的 `utcOffset` 拼法，格式化后是 `±HH:MM`）。
fn format_offset(seconds: i64) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

/// IANA 名（`Asia/Shanghai`）。拿不到就给 `tm_zone` 的缩写（`CST`）。
///
/// 标准库没有时区库，但 `/etc/localtime` 在 macOS 与 Linux 上都是指向
/// `.../zoneinfo/<Area>/<City>` 的符号链接，把 `zoneinfo/` 之后的部分取出来就是
/// `Intl` 返回的那个名字。
fn time_zone_name(zone_abbrev: Option<&str>) -> String {
    if let Ok(target) = std::fs::read_link("/etc/localtime") {
        let text = target.to_string_lossy();
        if let Some(index) = text.find("/zoneinfo/") {
            let name = &text[index + "/zoneinfo/".len()..];
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    if let Ok(tz) = std::env::var("TZ") {
        let name = tz.trim_start_matches(':');
        if !name.is_empty() {
            return name.to_string();
        }
    }
    zone_abbrev.unwrap_or_default().to_string()
}

/// `2026-11-12T07:39:25.246Z`（pi 用 `Date.toISOString()`，固定毫秒三位）。
fn iso8601_utc(now: std::time::SystemTime) -> String {
    let (secs, millis) = match now.duration_since(std::time::UNIX_EPOCH) {
        Ok(delta) => (delta.as_secs() as libc::time_t, delta.subsec_millis()),
        Err(_) => (0, 0),
    };
    let utc = utc_parts(secs);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        utc.year, utc.month, utc.day, utc.hour, utc.minute, utc.second, millis
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_match_pi_format() {
        assert_eq!(format_offset(8 * 3600), "+08:00");
        assert_eq!(format_offset(-(5 * 3600 + 30 * 60)), "-05:30");
        assert_eq!(format_offset(0), "+00:00");
    }

    #[test]
    fn iso8601_has_millisecond_precision() {
        let text = iso8601_utc(std::time::UNIX_EPOCH);
        assert_eq!(text, "1970-01-01T00:00:00.000Z");
    }

    #[test]
    fn snapshot_has_every_field_pi_returns() {
        let value = snapshot(std::time::SystemTime::now());
        for key in [
            "date",
            "time",
            "weekday",
            "timeZone",
            "utcOffset",
            "iso8601",
        ] {
            assert!(value.get(key).is_some(), "缺字段 {key}：{value}");
        }
        let weekday = value["weekday"].as_str().unwrap();
        assert!(WEEKDAYS.contains(&weekday), "{weekday}");
    }
}
