//! 应用更新检查：发布源固定为本项目，缓存成功结果并串行合并并发请求。

use std::cmp::Ordering;
use std::sync::OnceLock;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::Mutex;

const RELEASES_URL: &str = "https://api.github.com/repos/coder-nil/toiler/releases?per_page=20";
type Cache = Option<(Instant, Value)>;
static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();

#[derive(Debug)]
struct Version {
    core: [u64; 3],
    pre: Vec<String>,
    normalized: String,
}

impl Version {
    fn parse(raw: &str) -> Option<Self> {
        let raw = raw.trim().strip_prefix('v').unwrap_or(raw.trim());
        let (version, build) = raw.split_once('+').unwrap_or((raw, ""));
        if raw.contains('+') && !identifiers(build) {
            return None;
        }
        let (core, pre) = version.split_once('-').unwrap_or((version, ""));
        if version.contains('-') && !identifiers(pre) {
            return None;
        }
        let core: Vec<_> = core.split('.').collect();
        if core.len() != 3 || core.iter().any(|v| !numeric(v)) {
            return None;
        }
        let pre: Vec<String> = if pre.is_empty() {
            Vec::new()
        } else {
            pre.split('.').map(str::to_owned).collect()
        };
        if pre
            .iter()
            .any(|v| v.bytes().all(|b| b.is_ascii_digit()) && !numeric(v))
        {
            return None;
        }
        Some(Self {
            core: [
                core[0].parse().ok()?,
                core[1].parse().ok()?,
                core[2].parse().ok()?,
            ],
            pre,
            normalized: raw.to_owned(),
        })
    }

    fn compare(&self, other: &Self) -> Ordering {
        let core = self.core.cmp(&other.core);
        if core != Ordering::Equal {
            return core;
        }
        match (self.pre.is_empty(), other.pre.is_empty()) {
            (true, false) => return Ordering::Greater,
            (false, true) => return Ordering::Less,
            _ => {}
        }
        for (a, b) in self.pre.iter().zip(&other.pre) {
            let order = match (numeric(a), numeric(b)) {
                (true, true) => a.len().cmp(&b.len()).then_with(|| a.cmp(b)),
                (true, false) => Ordering::Less,
                (false, true) => Ordering::Greater,
                (false, false) => a.cmp(b),
            };
            if order != Ordering::Equal {
                return order;
            }
        }
        self.pre.len().cmp(&other.pre.len())
    }
}

fn numeric(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|b| b.is_ascii_digit())
        && (value.len() == 1 || !value.starts_with('0'))
}

fn identifiers(value: &str) -> bool {
    value
        .split('.')
        .all(|v| !v.is_empty() && v.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
}

fn response(releases: &Value) -> Result<Value, String> {
    let releases = releases
        .as_array()
        .ok_or("GitHub Releases 返回的结构不对")?;
    // GitHub 时间为 UTC ISO 字符串；使用会话层的实际日历算法，不能按每月 30 天近似。
    let latest = releases
        .iter()
        .filter_map(|release| {
            if release.get("draft").and_then(Value::as_bool) == Some(true) {
                return None;
            }
            let version = Version::parse(release.get("tag_name")?.as_str()?)?;
            let date = release
                .get("published_at")
                .and_then(Value::as_str)
                .or_else(|| release.get("created_at").and_then(Value::as_str))?;
            if !date.is_ascii() || !date.ends_with('Z') || date.len() > 32 {
                return None;
            }
            let timestamp = toiler_session::iso_to_ms(date)?;
            Some((timestamp, version))
        })
        .max_by_key(|(timestamp, _)| *timestamp);
    let (_, latest) = latest.ok_or("GitHub Releases 里没有可用版本")?;
    let current = env!("CARGO_PKG_VERSION");
    let newer = Version::parse(current).is_some_and(|v| latest.compare(&v) == Ordering::Greater);
    // 不信任上游任意 html_url，限制打开目标为本仓库发布页。
    Ok(json!({
        "currentVersion": current,
        "latestVersion": latest.normalized,
        "updateAvailable": newer,
        "releaseUrl": format!("https://github.com/coder-nil/toiler/releases/tag/v{}", latest.normalized),
    }))
}

pub async fn app_update(refresh: bool) -> Result<Value, String> {
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let requested_at = Instant::now();
    let mut cache = cache.lock().await;
    if let Some((checked_at, body)) = &*cache {
        if (!refresh && checked_at.elapsed() < Duration::from_secs(12 * 3600))
            || *checked_at >= requested_at
        {
            return Ok(body.clone());
        }
    }
    let result = async {
        let upstream = reqwest::Client::new()
            .get(RELEASES_URL)
            .timeout(Duration::from_secs(5))
            .header("accept", "application/vnd.github+json")
            .header("user-agent", "Toiler-Updater")
            .header("x-github-api-version", "2022-11-28")
            .send()
            .await
            .map_err(|e| format!("请求 GitHub Releases 失败：{e}"))?
            .error_for_status()
            .map_err(|e| format!("GitHub Releases 返回错误：{e}"))?;
        let text = upstream
            .text()
            .await
            .map_err(|e| format!("读取 GitHub Releases 失败：{e}"))?;
        let releases =
            serde_json::from_str(&text).map_err(|e| format!("GitHub Releases JSON 无效：{e}"))?;
        response(&releases)
    }
    .await;
    match result {
        Ok(body) => {
            *cache = Some((Instant::now(), body.clone()));
            Ok(body)
        }
        Err(_) if !refresh && cache.is_some() => Ok(cache.as_ref().unwrap().1.clone()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semver_validation_and_order() {
        for invalid in [
            "", "nightly", "1.2", "1.2.3.4", "01.2.3", "+1.2.3", "1.2.3-01", "1.2.3-", "1.2.3+",
        ] {
            assert!(Version::parse(invalid).is_none(), "{invalid}");
        }
        let ascending = [
            "1.0.0-alpha.9",
            "1.0.0-alpha.22",
            "1.0.0-rc.1",
            "1.0.0",
            "1.10.0",
            "2.0.0",
        ];
        for pair in ascending.windows(2) {
            assert_eq!(
                Version::parse(pair[0])
                    .unwrap()
                    .compare(&Version::parse(pair[1]).unwrap()),
                Ordering::Less
            );
        }
        assert_eq!(
            Version::parse("v1.0.0+a")
                .unwrap()
                .compare(&Version::parse("1.0.0+b").unwrap()),
            Ordering::Equal
        );
    }

    #[test]
    fn newest_release_uses_real_calendar_and_ignores_drafts() {
        let body = response(&json!([
            {"tag_name":"v1.0.0", "published_at":"2026-01-31T23:00:00Z"},
            {"tag_name":"v99.0.0", "published_at":"2026-02-01T00:00:00Z", "html_url":"https://evil.invalid"},
            {"tag_name":"v100.0.0", "published_at":"2026-03-01T00:00:00Z", "draft":true},
            {"tag_name":"nightly", "published_at":"2026-04-01T00:00:00Z"}
        ])).unwrap();
        assert_eq!(body["latestVersion"], "99.0.0");
        assert_eq!(body["updateAvailable"], true);
        assert_eq!(
            body["releaseUrl"],
            "https://github.com/coder-nil/toiler/releases/tag/v99.0.0"
        );
        assert!(response(&json!([])).is_err());
    }
}
