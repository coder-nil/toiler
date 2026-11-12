//! ISO-8601 ↔ unix 毫秒。
//!
//! pi 的 v3 会话文件用 `2026-11-10T06:41:44.431Z` 这种字符串，
//! rpi 的条目层用 `i64` 毫秒。两边互转不能引 chrono（rpi 自己就是手写的），
//! 这里照 rpi `jsonl::repo` 的做法用 Howard Hinnant 的算法手写。

/// `2026-11-10T06:41:44.431Z` → `1794292904431`。
///
/// 容忍：无毫秒（`.Z`）、毫秒位数不足或超过 3 位、以 `+00:00` 结尾之外的
/// 非 Z 时区暂不支持（pi 只写 Z）。
pub fn iso_to_ms(iso: &str) -> Option<i64> {
    let (date, rest) = iso.split_once('T')?;
    let mut d = date.split('-');
    let y: i64 = d.next()?.parse().ok()?;
    let mo: i64 = d.next()?.parse().ok()?;
    let da: i64 = d.next()?.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&da) {
        return None;
    }

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

    // days_from_civil
    let y2 = if mo <= 2 { y - 1 } else { y };
    let era = if y2 >= 0 { y2 } else { y2 - 399 } / 400;
    let yoe = y2 - era * 400;
    let mp = (mo + 9) % 12;
    let doy = (153 * mp + 2) / 5 + da - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146097 + doe - 719468;

    Some(((days * 86400) + h * 3600 + mi * 60 + s) * 1000 + ms)
}

/// `1794292904431` → `2026-11-10T06:41:44.431Z`（pi 的 `toISOString()` 形状）。
pub fn ms_to_iso(ms: i64) -> String {
    let secs = ms.div_euclid(1000);
    let millis = ms.rem_euclid(1000);
    let days = secs.div_euclid(86400);
    let rem = secs.rem_euclid(86400);
    let (h, mi, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);

    // civil_from_days
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrips_real_pi_timestamps() {
        for iso in [
            "2026-11-10T06:41:44.431Z",
            "2026-01-01T00:00:00.000Z",
            "2026-08-17T07:18:56.979Z",
            "2024-02-29T23:59:59.999Z",
        ] {
            let ms = iso_to_ms(iso).unwrap_or_else(|| panic!("parse {iso}"));
            assert_eq!(ms_to_iso(ms), iso);
        }
    }

    #[test]
    fn epoch_and_leap_boundaries() {
        assert_eq!(iso_to_ms("1970-01-01T00:00:00.000Z"), Some(0));
        assert_eq!(ms_to_iso(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            iso_to_ms("2026-03-01T00:00:00Z"),
            iso_to_ms("2026-03-01T00:00:00.000Z")
        );
    }

    #[test]
    fn rejects_garbage() {
        assert_eq!(iso_to_ms("not a timestamp"), None);
        assert_eq!(iso_to_ms("2026-13-01T00:00:00.000Z"), None);
    }
}
