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
    // SAFETY: 两个平台都把 tm 已零初始化后交给各自的 `localtime_*`；
    // Windows 没有 `localtime_r`（见 `libc` 的 `windows` 模块）。
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    if !fill_local_tm(secs, &mut tm) {
        // 极端情况下（例如 TZ 指向坏文件）退回 UTC，至少不要 panic。
        let utc = utc_parts(secs);
        return LocalTime {
            offset_seconds: 0,
            zone_abbrev: None,
            ..utc
        };
    }
    let zone_abbrev = zone_abbrev_of(&tm);
    LocalTime {
        year: tm.tm_year + 1900,
        month: (tm.tm_mon + 1) as u32,
        day: tm.tm_mday as u32,
        hour: tm.tm_hour as u32,
        minute: tm.tm_min as u32,
        second: tm.tm_sec as u32,
        weekday: (tm.tm_wday.clamp(0, 6)) as usize,
        offset_seconds: utc_offset_seconds(secs, &tm),
        zone_abbrev,
    }
}

/// 把 `secs` 对应的时间填进本地的 `tm`。失败返回 `false`。
///
/// Windows 的 libc 只有 `localtime_s`（参数顺序与 POSIX 的 `localtime_r` 相反，
/// 且返回 errno 而不是指针），且 `tm` 里**没有** `tm_gmtoff` / `tm_zone` 两个字段
/// —— 所以下面所有读 `tm_gmtoff` / `tm_zone` 的地方都要按平台分支。
#[cfg(unix)]
fn fill_local_tm(secs: libc::time_t, tm: &mut libc::tm) -> bool {
    // SAFETY: `localtime_r` 只写我们给的 tm。
    !unsafe { libc::localtime_r(&secs, tm) }.is_null()
}

#[cfg(not(unix))]
fn fill_local_tm(secs: libc::time_t, tm: &mut libc::tm) -> bool {
    // SAFETY: `localtime_s` 只写第一个参数指向的 tm，0 表示成功。
    unsafe { libc::localtime_s(tm, &secs) == 0 }
}

/// 时区缩写（`CST`）。Windows 的 `tm` 没有 `tm_zone`，只能不给。
#[cfg(unix)]
fn zone_abbrev_of(tm: &libc::tm) -> Option<String> {
    // SAFETY: 成功返回的 tm_zone 指向静态字符串或 NULL。
    let ptr = tm.tm_zone;
    if ptr.is_null() {
        return None;
    }
    Some(
        unsafe { std::ffi::CStr::from_ptr(ptr) }
            .to_string_lossy()
            .to_string(),
    )
}

#[cfg(not(unix))]
fn zone_abbrev_of(_tm: &libc::tm) -> Option<String> {
    None
}

/// 本地时区相对 UTC 的偏移秒数。
///
/// unix 直接拿 `tm_gmtoff`。非 unix 既没有这个字段、`libc` 也没导出 `mktime`
/// （Windows 只给 `_time64` / `_localtime64_s` / `_gmtime64_s`），所以用两个
/// 已归一化的 `tm` 在「同一天内」相减反推偏移：本地 tm 与 UTC tm 的
/// 「日内秒数差」就是偏移，跨日的差值用 24h 取模回落。
#[cfg(unix)]
fn utc_offset_seconds(_secs: libc::time_t, tm: &libc::tm) -> i64 {
    // `tm_gmtoff` 的字面类型按平台不同（macOS 是 i64，Linux 是 i32），
    // 统一转 i64 才两边都能编。
    #[allow(clippy::unnecessary_cast)]
    {
        tm.tm_gmtoff as i64
    }
}

#[cfg(not(unix))]
fn utc_offset_seconds(secs: libc::time_t, local: &libc::tm) -> i64 {
    let mut utc: libc::tm = unsafe { std::mem::zeroed() };
    if !fill_utc_tm(secs, &mut utc) {
        return 0;
    }
    let local_secs = local.tm_hour as i64 * 3600 + local.tm_min as i64 * 60 + local.tm_sec as i64;
    let utc_secs = utc.tm_hour as i64 * 3600 + utc.tm_min as i64 * 60 + utc.tm_sec as i64;
    // 都是同一天的墙上时间。差值可能跨日（例如 +08:00 时 03:00 本地 ↔ 19:00 UTC）。
    let diff = local_secs - utc_secs;
    const HALF_DAY: i64 = 12 * 3600;
    if diff > HALF_DAY {
        diff - 24 * 3600
    } else if diff < -HALF_DAY {
        diff + 24 * 3600
    } else {
        diff
    }
}

fn utc_parts(secs: libc::time_t) -> LocalTime {
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: 两个分支都只写我们给的 tm；Windows 的 `gmtime_s` 返回 errno。
    if !fill_utc_tm(secs, &mut tm) {
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

/// 把 `secs` 对应的 UTC 时间填进 `tm`。失败返回 `false`。
#[cfg(unix)]
fn fill_utc_tm(secs: libc::time_t, tm: &mut libc::tm) -> bool {
    // SAFETY: `gmtime_r` 只写 tm。
    !unsafe { libc::gmtime_r(&secs, tm) }.is_null()
}

#[cfg(not(unix))]
fn fill_utc_tm(secs: libc::time_t, tm: &mut libc::tm) -> bool {
    // SAFETY: `gmtime_s` 只写第一个参数指向的 tm，0 表示成功。
    unsafe { libc::gmtime_s(tm, &secs) == 0 }
}

/// `+08:00` / `-05:30`（pi 用 `Intl` 的 `utcOffset` 拼法，格式化后是 `±HH:MM`）。
fn format_offset(seconds: i64) -> String {
    let sign = if seconds < 0 { '-' } else { '+' };
    let abs = seconds.unsigned_abs();
    format!("{sign}{:02}:{:02}", abs / 3600, (abs % 3600) / 60)
}

/// 时区名。unix 尽量给 IANA 名（`Asia/Shanghai`），拿不到就退回缩写。
///
/// Windows 没有 `/etc/localtime`，也没有 `TZ` 的 IANA 语义，所以另外走一条路：
/// `tzutil /g` 拿系统时区 ID（形如 `China Standard Time`）。它不是 IANA 名，
/// 但比返回空串有用得多——前端的展示与提示都靠这个字段。
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
    if let Some(name) = platform_time_zone() {
        return name;
    }
    zone_abbrev.unwrap_or_default().to_string()
}

/// 平台自己的时区名。
///
/// Windows：`tzutil /g` 输出一行时区 ID（`China Standard Time`）。
/// 不直接读写注册表：`tzutil` 是官方命令行入口，不需要开额外的依赖或 FFI。
#[cfg(windows)]
fn platform_time_zone() -> Option<String> {
    let output = std::process::Command::new("tzutil")
        .arg("/g")
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let name = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!name.is_empty()).then_some(name)
}

#[cfg(not(windows))]
fn platform_time_zone() -> Option<String> {
    None
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
