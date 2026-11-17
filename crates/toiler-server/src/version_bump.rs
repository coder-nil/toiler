//! 版本号升级：把散落在 8 个文件里的版本号同步到下一个版本，并（桌面壳内）提交 + 打标签。
//!
//! 移植自 `tools/bump-version.py` 的 `desktop_release`。**为什么要有 Rust 版**：
//! 客户端机器不保证装了 Python 3.9+，而「版本号升级」是桌面 Git 面板的三个按钮
//! （patch / minor / major），属于产品功能而不是开发脚本。原来靠 `python3 -c` 跑
//! 内嵌脚本，没 Python 的机器上三个按钮直接不可用。
//!
//! `tools/bump-version.py` 仍保留给开发者做 CLI 用（`--check` / `--dry-run` /
//! `--since`）。两份实现的一致性由 `version_bump::tests` 里的规则用例守着：
//! 语义（语义化版本递增、预发布转正、提交分类）必须完全一致。
//!
//! 版本号散落位置（与 py 脚本同一份清单，改这里也要改那边）：
//!
//! | 文件 | 位置 |
//! |---|---|
//! | `crates/toiler-desktop/tauri.conf.json` | 顶层 `version`（**基数**，以它为准） |
//! | `crates/toiler-{poc,session,server,desktop}/Cargo.toml` | `[package]` 的 `version` |
//! | `Cargo.lock` | 4 个本地包的 `version` |
//! | `frontend/package.json` / `package-lock.json` | 顶层与 `packages[""]` 的 `version` |
//! | `CHANGELOG.md` / `frontend/data/changelog.json` | 新增版本段 |
//!
//! ## 两条不能碰的约束
//!
//! 1. **`tauri.conf.json` 走文本替换，不 JSON 往返**：它的 `capabilities` 数组是
//!    单行手写格式，`serde_json` 会摊成多行，产生无关 diff。
//! 2. **`data/changelog.json` 不能由 `CHANGELOG.md` 重新生成**：两者历史条目
//!    已有漂移（条目顺序、重复项不同），重写会产生无关 diff。只能各插一段。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// 版本号清单（与 `tools/bump-version.py` 的常量一致）。
const TAURI_CONF: &str = "crates/toiler-desktop/tauri.conf.json";
const CARGO_TOMLS: [&str; 4] = [
    "crates/toiler-poc/Cargo.toml",
    "crates/toiler-session/Cargo.toml",
    "crates/toiler-server/Cargo.toml",
    "crates/toiler-desktop/Cargo.toml",
];
const CARGO_LOCK: &str = "Cargo.lock";
const PACKAGE_JSON: &str = "crates/toiler-desktop/frontend/package.json";
const PACKAGE_LOCK: &str = "crates/toiler-desktop/frontend/package-lock.json";
const CHANGELOG_MD: &str = "CHANGELOG.md";
const CHANGELOG_JSON: &str = "crates/toiler-desktop/frontend/data/changelog.json";

/// `Cargo.lock` 里要改的本地包名。
const LOCAL_CRATES: [&str; 4] = [
    "toiler-desktop",
    "toiler-poc",
    "toiler-server",
    "toiler-session",
];

/// 三个按钮对应的递增级别。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Patch,
    Minor,
    Major,
}

impl Level {
    pub fn as_str(self) -> &'static str {
        match self {
            Level::Patch => "patch",
            Level::Minor => "minor",
            Level::Major => "major",
        }
    }
}

/// 可预期的失败：直接作为中文提示返回给界面。
#[derive(Debug)]
pub struct BumpError(pub String);

impl std::fmt::Display for BumpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for BumpError {}

type Result<T> = std::result::Result<T, BumpError>;

fn fail<T>(message: impl Into<String>) -> Result<T> {
    Err(BumpError(message.into()))
}

// ---------------------------------------------------------------------------
// semver
// ---------------------------------------------------------------------------

/// 解析后的语义化版本。`pre` 为空表示正式版。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Semver {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
    pub pre: String,
}

impl Semver {
    /// 接受 `1.2.3` / `1.2.3-alpha.1` / `v1.2.3`（写回时统一去掉 `v`）。
    ///
    /// 与 py 脚本的 `SEMVER` 正则逐字对齐：`^v?(\d+)\.(\d+)\.(\d+)(?:-([0-9A-Za-z.-]+))?$`。
    pub fn parse(raw: &str) -> Result<Self> {
        let text = raw.trim();
        let text = text.strip_prefix('v').unwrap_or(text);
        let (core, pre) = match text.split_once('-') {
            Some((core, pre)) => {
                // `1.2.3-` 是空预发布，不是合法 semver（py 正则要求至少 1 个字符）。
                if pre.is_empty() {
                    return fail(format!(
                        "`{raw}` 不是合法的 semver（期望 `1.2.3` 或 `1.2.3-alpha.1`）"
                    ));
                }
                (core, pre.to_string())
            }
            None => (text, String::new()),
        };
        if !pre.is_empty()
            && !pre
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-')
        {
            return fail(format!(
                "`{raw}` 不是合法的 semver（期望 `1.2.3` 或 `1.2.3-alpha.1`）"
            ));
        }
        let parts: Vec<&str> = core.split('.').collect();
        if parts.len() != 3 {
            return fail(format!(
                "`{raw}` 不是合法的 semver（期望 `1.2.3` 或 `1.2.3-alpha.1`）"
            ));
        }
        let number = |s: &str| -> Result<u64> {
            if s.is_empty() || !s.chars().all(|c| c.is_ascii_digit()) {
                return fail(format!(
                    "`{raw}` 不是合法的 semver（期望 `1.2.3` 或 `1.2.3-alpha.1`）"
                ));
            }
            s.parse::<u64>()
                .map_err(|_| BumpError(format!("`{raw}` 的版本号超出范围")))
        };
        Ok(Self {
            major: number(parts[0])?,
            minor: number(parts[1])?,
            patch: number(parts[2])?,
            pre,
        })
    }

    pub fn to_string_plain(&self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.patch)
    }

    /// 渲染成写回文件的形状（含预发布后缀）。
    pub fn render(&self) -> String {
        if self.pre.is_empty() {
            self.to_string_plain()
        } else {
            format!("{}-{}", self.to_string_plain(), self.pre)
        }
    }
}

/// 下一版本与一句中文说明。
///
/// **预发布转正是关键规则**：`1.2.3-beta.1` 的 patch 递增不是 `1.2.4`，而是
/// `1.2.3`（把预发布转成正式版）。py 脚本同样如此。
pub fn next_version(current: &Semver, level: Level) -> (Semver, String) {
    match level {
        Level::Patch => {
            if !current.pre.is_empty() {
                let next = Semver {
                    major: current.major,
                    minor: current.minor,
                    patch: current.patch,
                    pre: String::new(),
                };
                (
                    next.clone(),
                    format!("预发布转正：{} → {}", current.render(), next.render()),
                )
            } else {
                let next = Semver {
                    patch: current.patch + 1,
                    ..current.clone()
                };
                (
                    next.clone(),
                    format!("patch 递增：{} → {}", current.render(), next.render()),
                )
            }
        }
        Level::Minor => {
            let next = Semver {
                major: current.major,
                minor: current.minor + 1,
                patch: 0,
                pre: String::new(),
            };
            (
                next.clone(),
                format!("minor 递增：{} → {}", current.render(), next.render()),
            )
        }
        Level::Major => {
            let next = Semver {
                major: current.major + 1,
                minor: 0,
                patch: 0,
                pre: String::new(),
            };
            (
                next.clone(),
                format!("major 递增：{} → {}", current.render(), next.render()),
            )
        }
    }
}

// ---------------------------------------------------------------------------
// 提交分类（变更记录）
// ---------------------------------------------------------------------------

/// 一个发布提交：短哈希 + 主题 + 正文细节。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReleaseCommit {
    pub hash: String,
    pub subject: String,
    pub details: String,
}

/// 变更记录的一节。
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    pub title: &'static str,
    pub kind: &'static str,
    pub items: Vec<String>,
}

/// 把提交按 conventional-commit 前缀分成 Added / Changed / Fixed。
///
/// 与 py 的 `changelog_sections` 同一套规则：`feat` → Added，`fix` → Fixed，
/// 其余（含无前缀、合并提交）→ Changed。未分类的兜底是 Changed，不能丢。
pub fn changelog_sections(commits: &[ReleaseCommit], new: &str) -> Vec<Section> {
    let mut added = Vec::new();
    let mut changed = Vec::new();
    let mut fixed = Vec::new();

    for commit in commits {
        let kind = conventional_kind(&commit.subject);
        let title = match kind {
            "feat" => &mut added,
            "fix" => &mut fixed,
            _ => &mut changed,
        };
        let mut item = format!("{}（`{}`）", commit.subject, short_hash(&commit.hash));
        if !commit.details.is_empty() {
            item.push_str(&format!("：{}", commit.details));
        }
        title.push(item);
    }

    changed.push(format!("应用、npm、Tauri 和 Cargo 版本号统一为 `{new}`。"));

    let mut sections = Vec::new();
    for (name, items) in [("Added", added), ("Changed", changed), ("Fixed", fixed)] {
        if items.is_empty() {
            continue;
        }
        sections.push(Section {
            title: name,
            kind: match name {
                "Added" => "added",
                "Fixed" => "fixed",
                _ => "changed",
            },
            items,
        });
    }
    sections
}

/// `feat(scope)!: x` → `feat`；没有 conventional 前缀时返回空串。
pub fn conventional_kind(subject: &str) -> &str {
    let Some(colon) = subject.find(':') else {
        return "";
    };
    let head = &subject[..colon];
    let head = head.strip_suffix('!').unwrap_or(head);
    let name = match head.split_once('(') {
        // `feat(scope)` —— scope 必须闭合在冒号前。
        Some((name, rest)) => {
            if !rest.ends_with(')') {
                return "";
            }
            name
        }
        None => head,
    };
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_lowercase()) {
        name
    } else {
        ""
    }
}

fn short_hash(hash: &str) -> String {
    hash.chars().take(12).collect()
}

// ---------------------------------------------------------------------------
// Cargo.lock 的最小文本改写（不依赖 cargo 可执行）
// ---------------------------------------------------------------------------

/// 把 `Cargo.lock` 里 4 个本地包的 version 改写为 `new`。
///
/// 按 py 脚本同一策略：**文本替换而不是 cargo update**。桌面壳里不能假设机器
/// 装了 cargo（用户的机器只有 .app），而 `cargo update` 还会碰网络。
/// 只认 `name = "..."` 紧跟 `version = "..."` 的包条目，每包必须恰好 1 处。
pub fn rewrite_cargo_lock(text: &str, new: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    // 当前正在看的包条目：只有本地包才等它的 version 行。
    let mut current: Option<&'static str> = None;
    let mut replaced: Vec<&'static str> = Vec::new();

    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);

        if let Some(rest) = trimmed.strip_prefix("name = \"") {
            if let Some(found) = rest.strip_suffix('"') {
                current = LOCAL_CRATES.iter().copied().find(|c| *c == found);
            }
            out.push_str(line);
            continue;
        }

        if let Some(name) = current {
            if let Some(rest) = trimmed.strip_prefix("version = \"") {
                if rest.ends_with('"') {
                    out.push_str(&format!("version = \"{new}\""));
                    if line.ends_with('\n') {
                        out.push('\n');
                    }
                    replaced.push(name);
                    current = None;
                    continue;
                }
            }
        }

        // 空行或新的 [[package]] 结束当前条目。
        if trimmed.is_empty() || trimmed.starts_with('[') {
            current = None;
        }
        out.push_str(line);
    }

    for name in LOCAL_CRATES {
        let count = replaced.iter().filter(|c| **c == name).count();
        if count != 1 {
            return fail(format!(
                "Cargo.lock 里 {name} 版本行替换了 {count} 处，应为 1 处"
            ));
        }
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// 计划（纯计算，不落盘）
// ---------------------------------------------------------------------------

/// 一次待写入。
#[derive(Debug, Clone)]
pub struct PlannedEdit {
    pub path: PathBuf,
    pub rel: String,
    pub content: String,
}

/// 升级计划：所有待改文件 + 元信息。
#[derive(Debug, Clone)]
pub struct Plan {
    pub current: String,
    pub version: String,
    pub tag: String,
    pub head: String,
    pub plan_id: String,
    pub files: Vec<String>,
    pub edits: Vec<PlannedEdit>,
    pub sections: Vec<Section>,
    /// 变更记录预览（Markdown 片段），给界面展示。
    pub changelog_md: String,
}

impl Plan {
    /// 给界面的预览 JSON（与 py 的 `desktop_release(dry_run=True)` 同形）。
    pub fn preview_json(&self) -> Value {
        json!({
            "current": self.current,
            "version": self.version,
            "tag": self.tag,
            "head": self.head,
            "planId": self.plan_id,
            "files": self.files,
            "output": self.changelog_md,
        })
    }
}

/// 今日日期（本地时区），`YYYY-MM-DD`。变更记录用。
pub fn today_local() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let offset = local_utc_offset_seconds();
    let secs = seconds + i64::from(offset);
    let days = secs.div_euclid(86_400);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// 当前时区相对 UTC 的偏移秒数（拿不到按 0 = UTC）。与 `mobile_view` 同一做法。
fn local_utc_offset_seconds() -> i32 {
    unsafe {
        let now = libc::time(std::ptr::null_mut());
        let mut local: libc::tm = std::mem::zeroed();
        #[cfg(unix)]
        {
            if libc::localtime_r(&now, &mut local).is_null() {
                return 0;
            }
            local.tm_gmtoff as i32
        }
        #[cfg(not(unix))]
        {
            if libc::localtime_s(&mut local, &now) != 0 {
                return 0;
            }
            let mut utc: libc::tm = std::mem::zeroed();
            if libc::gmtime_s(&mut utc, &now) != 0 {
                return 0;
            }
            let local_secs =
                local.tm_hour as i64 * 3600 + local.tm_min as i64 * 60 + local.tm_sec as i64;
            let utc_secs = utc.tm_hour as i64 * 3600 + utc.tm_min as i64 * 60 + utc.tm_sec as i64;
            let mut diff = local_secs - utc_secs;
            if diff > 12 * 3600 {
                diff -= 86_400;
            } else if diff < -12 * 3600 {
                diff += 86_400;
            }
            diff as i32
        }
    }
}

/// 天数（1970-01-01 起）→ 公历年月日。与 `toiler_session::ms_to_iso` 同一算法。
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m as u32, d as u32)
}

// ---------------------------------------------------------------------------
// 读取与校验
// ---------------------------------------------------------------------------

/// `tauri.conf.json` 的顶层 version —— **基数**，发布流水线校验 tag 与它一致。
///
/// 走文本匹配而不是 JSON 解析：这份文件的 `capabilities` 数组是单行手写格式，
/// `serde_json` 往返会把它摊成多行，产生无关 diff。
fn read_tauri_version(text: &str) -> Result<String> {
    let mut found: Vec<String> = Vec::new();
    for line in text.lines() {
        // 顶层字段缩进两格；`version` 出现在更深缩进处（如 bundle 内）不算。
        if let Some(rest) = line.strip_prefix("  \"version\": \"") {
            if let Some(value) = rest.strip_suffix("\",") {
                found.push(value.to_string());
            }
        }
    }
    if found.len() != 1 {
        return fail(format!(
            "{TAURI_CONF} 里找到 {} 处顶层 version，应为 1 处",
            found.len()
        ));
    }
    Ok(found.remove(0))
}

/// 把一个 `Cargo.toml` 里 `[package]` 段的 version 行改掉。
///
/// 用 `name = "toiler-xxx"` 作锚点：直接替换 `version = "..."` 会误伤
/// `[dependencies]` 里的同名键。
fn rewrite_cargo_toml(text: &str, new: &str) -> Result<String> {
    let lines: Vec<&str> = text.lines().collect();
    let mut out: Vec<String> = Vec::with_capacity(lines.len());
    let mut replaced = 0usize;
    let mut i = 0usize;
    while i < lines.len() {
        let line = lines[i];
        out.push(line.to_string());
        // `name = "toiler-*"` 紧跟着的那一行才是 `[package]` 的 version。
        if line.starts_with("name = \"toiler-") && line.ends_with('"') {
            if let Some(next) = lines.get(i + 1) {
                if let Some(rest) = next.strip_prefix("version = \"") {
                    if rest.ends_with('"') {
                        out.push(format!("version = \"{new}\""));
                        replaced += 1;
                        i += 2;
                        continue;
                    }
                }
            }
        }
        i += 1;
    }
    if replaced != 1 {
        return fail(format!("Cargo.toml 版本行替换了 {replaced} 处，应为 1 处"));
    }
    Ok(out.join("\n") + if text.ends_with('\n') { "\n" } else { "" })
}

/// 按 2 空格缩进 + 末尾换行序列化（与两份 package 文件现有格式逐字节一致）。
fn dump_json(value: &Value) -> String {
    serde_json::to_string_pretty(value).expect("序列化 JSON") + "\n"
}

// ---------------------------------------------------------------------------
// 计划
// ---------------------------------------------------------------------------

/// 在 `root` 里生成升级计划。**纯计算，不落盘**。
///
/// `commits` 是发布提交范围（由调用方用 git 读出）；变更记录按它分类。
pub fn plan(root: &Path, level: Level, commits: &[ReleaseCommit]) -> Result<Plan> {
    // 先确认这是本项目仓库根（四个 Cargo.toml 与 tauri.conf.json 都在）。
    if !root.join("Cargo.toml").is_file() {
        return fail(format!(
            "{} 不像仓库根目录（缺 Cargo.toml）",
            root.display()
        ));
    }

    // 防项目里的符号链接把版本写入引到仓库外。
    let canonical_root = root
        .canonicalize()
        .map_err(|e| BumpError(format!("无法解析仓库根目录：{e}")))?;
    let mut paths: Vec<&str> = Vec::new();
    paths.extend(CARGO_TOMLS);
    paths.push(TAURI_CONF);
    paths.push(PACKAGE_JSON);
    paths.push(PACKAGE_LOCK);
    paths.push(CHANGELOG_MD);
    paths.push(CHANGELOG_JSON);
    paths.push(CARGO_LOCK);

    for rel in &paths {
        let path = root.join(rel);
        if !path.is_file() {
            return fail(format!(
                "项目缺少版本文件：{rel}，只支持与 bump-version 兼容的项目"
            ));
        }
        let real = path
            .canonicalize()
            .map_err(|e| BumpError(format!("无法解析 {rel}：{e}")))?;
        if !real.starts_with(&canonical_root) {
            return fail(format!("版本文件指向仓库外：{rel}"));
        }
    }

    let read = |rel: &str| -> Result<String> {
        std::fs::read_to_string(root.join(rel))
            .map_err(|e| BumpError(format!("读取 {rel} 失败：{e}")))
    };

    let current_raw = read_tauri_version(&read(TAURI_CONF)?)?;
    let current = Semver::parse(&current_raw)?;
    let (next, _) = next_version(&current, level);
    let new = next.render();
    let tag = format!("v{new}");

    // ---- 变更记录 ----
    let changelog_md_text = read(CHANGELOG_MD)?;
    if changelog_md_text.contains(&format!("## [{new}]")) {
        return fail(format!(
            "CHANGELOG.md 里已存在 `## [{new}]` 段，换个版本号或手工处理"
        ));
    }
    let anchor = changelog_md_text.find("## [").ok_or_else(|| {
        BumpError("CHANGELOG.md 里找不到任何 `## [版本]` 段，无法确定插入位置".into())
    })?;
    let mut releases: Value = serde_json::from_str(&read(CHANGELOG_JSON)?)
        .map_err(|e| BumpError(format!("{CHANGELOG_JSON} 不是合法 JSON：{e}")))?;
    if releases.as_array().is_some_and(|list| {
        list.iter()
            .any(|r| r.get("version").and_then(Value::as_str) == Some(new.as_str()))
    }) {
        return fail(format!("data/changelog.json 里已存在 {new} 条目"));
    }

    let sections = changelog_sections(commits, &new);
    let today = today_local();
    let mut block = format!("## [{new}] - {today}\n\n");
    for section in &sections {
        block.push_str(&format!("### {}\n\n", section.title));
        for item in &section.items {
            block.push_str(&format!("- {item}\n"));
        }
        block.push('\n');
    }
    let new_md = format!(
        "{}{}{}",
        &changelog_md_text[..anchor],
        block,
        &changelog_md_text[anchor..]
    );
    let entry = json!({
        "version": new,
        "date": today,
        "sections": sections,
    });
    if let Some(list) = releases.as_array_mut() {
        list.insert(0, entry);
    }
    let new_json = dump_json(&releases);

    // ---- 各版本文件 ----
    let mut edits: Vec<PlannedEdit> = Vec::new();
    for rel in CARGO_TOMLS {
        let text = read(rel)?;
        edits.push(PlannedEdit {
            path: root.join(rel),
            rel: rel.to_string(),
            content: rewrite_cargo_toml(&text, &new)?,
        });
    }
    let tauri_text = read(TAURI_CONF)?;
    edits.push(PlannedEdit {
        path: root.join(TAURI_CONF),
        rel: TAURI_CONF.to_string(),
        content: rewrite_tauri_version(&tauri_text, &new)?,
    });

    // 这两份 JSON 逐字节安全（2 空格 + 末尾换行），走结构化改写。
    let mut pkg: Value = serde_json::from_str(&read(PACKAGE_JSON)?)
        .map_err(|e| BumpError(format!("{PACKAGE_JSON} 不是合法 JSON：{e}")))?;
    pkg["version"] = json!(new);
    edits.push(PlannedEdit {
        path: root.join(PACKAGE_JSON),
        rel: PACKAGE_JSON.to_string(),
        content: dump_json(&pkg),
    });

    let mut lock: Value = serde_json::from_str(&read(PACKAGE_LOCK)?)
        .map_err(|e| BumpError(format!("{PACKAGE_LOCK} 不是合法 JSON：{e}")))?;
    lock["version"] = json!(new);
    if let Some(root_pkg) = lock.get_mut("packages").and_then(|p| p.get_mut("")) {
        root_pkg["version"] = json!(new);
    }
    edits.push(PlannedEdit {
        path: root.join(PACKAGE_LOCK),
        rel: PACKAGE_LOCK.to_string(),
        content: dump_json(&lock),
    });

    edits.push(PlannedEdit {
        path: root.join(CHANGELOG_MD),
        rel: CHANGELOG_MD.to_string(),
        content: new_md,
    });
    edits.push(PlannedEdit {
        path: root.join(CHANGELOG_JSON),
        rel: CHANGELOG_JSON.to_string(),
        content: new_json,
    });

    // Cargo.lock 只算内容，落盘时机与其它文件一致；纳入 planId 让预览能覆盖它。
    let lock_text = read(CARGO_LOCK)?;
    let lock_new = rewrite_cargo_lock(&lock_text, &new)?;
    edits.push(PlannedEdit {
        path: root.join(CARGO_LOCK),
        rel: CARGO_LOCK.to_string(),
        content: lock_new,
    });

    // planId：所有 (相对路径, 内容) 的 sha256。确认时比对，防止预览后被改动。
    let plan_id = {
        use sha2::{Digest, Sha256};
        let mut hasher = Sha256::new();
        for edit in &edits {
            hasher.update(edit.rel.as_bytes());
            hasher.update([0u8]);
            hasher.update(edit.content.as_bytes());
            hasher.update([0u8]);
        }
        let digest = hasher.finalize();
        digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };

    Ok(Plan {
        current: current.render(),
        version: new,
        tag,
        // head 由调用方填（需要 git）。
        head: String::new(),
        plan_id,
        files: paths.iter().map(|p| p.to_string()).collect(),
        edits,
        sections,
        changelog_md: block,
    })
}

/// 改写 `tauri.conf.json` 顶层 version（文本替换，不动其它字节）。
fn rewrite_tauri_version(text: &str, new: &str) -> Result<String> {
    let mut out = String::with_capacity(text.len());
    let mut replaced = 0usize;
    for line in text.split_inclusive('\n') {
        let trimmed = line.trim_end_matches(['\n', '\r']);
        if let Some(rest) = trimmed.strip_prefix("  \"version\": \"") {
            if rest.ends_with("\",") {
                out.push_str(&format!("  \"version\": \"{new}\","));
                if line.ends_with('\n') {
                    out.push('\n');
                }
                replaced += 1;
                continue;
            }
        }
        out.push_str(line);
    }
    if replaced != 1 {
        return fail(format!("{TAURI_CONF} 版本替换了 {replaced} 处，应为 1 处"));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

// ---------------------------------------------------------------------------
// git 交互与落盘（桌面壳专属路径）
// ---------------------------------------------------------------------------

use std::time::Duration;

use crate::gitsvc;

const GIT_TIMEOUT: Duration = Duration::from_secs(60);

fn git(root: &Path, args: &[&str]) -> Result<String> {
    gitsvc::run_git(root, args, GIT_TIMEOUT).map_err(|e| BumpError(e.message))
}

/// 读出发布提交范围：最近可达的 `v<semver>` 标签之后的所有提交（含合并及其分支）。
///
/// 与 py 的 `release_commits` 同一策略：`describe` 按提交图距离找最近标签，
/// 不按路径过滤、不只看 first-parent（否则合并进来的内容会漏）。
pub fn release_commits(root: &Path, since: Option<&str>) -> Result<Vec<ReleaseCommit>> {
    if git(root, &["rev-parse", "--is-shallow-repository"])? == "true" {
        return fail("浅克隆无法保证发布历史完整，请先获取完整历史和标签后重试");
    }
    let head = git(root, &["rev-parse", "HEAD"])?;
    let tags: Vec<String> = git(root, &["tag", "--merged", &head])?
        .lines()
        .filter(|t| t.starts_with('v') && Semver::parse(t).is_ok())
        .map(str::to_string)
        .collect();

    let base = match since {
        Some(tag) => {
            if !tags.iter().any(|t| t == tag) {
                return fail(format!("`{tag}` 不是当前 HEAD 可达的 v<semver> 发布标签"));
            }
            Some(tag.to_string())
        }
        None if !tags.is_empty() => {
            let mut args = vec!["describe", "--tags", "--abbrev=0"];
            // 先按可达标签过滤，避免 describe 选中别的分支上的标签。
            let matches: Vec<String> = tags.iter().map(|t| format!("--match={t}")).collect();
            for m in &matches {
                args.push(m);
            }
            args.push(&head);
            Some(git(root, &args)?)
        }
        None => None,
    };

    let revision = match &base {
        Some(tag) => {
            let base_commit = git(root, &["rev-parse", &format!("refs/tags/{tag}^{{commit}}")])?;
            format!("{base_commit}..{head}")
        }
        None => head.clone(),
    };

    let log = git(
        root,
        &["log", "--reverse", "--format=%H%x00%B%x00", &revision, "--"],
    )?;
    let mut commits = Vec::new();
    let fields: Vec<&str> = log.split('\0').collect();
    let mut i = 0;
    while i + 1 < fields.len() {
        let hash = fields[i].trim();
        let message = fields[i + 1].trim();
        i += 2;
        if hash.is_empty() {
            continue;
        }
        let mut lines = message.lines();
        let subject = lines.next().unwrap_or("（无提交说明）").trim().to_string();
        let details = lines
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("；");
        commits.push(ReleaseCommit {
            hash: hash.to_string(),
            subject,
            details,
        });
    }
    Ok(commits)
}

/// 升级请求（HTTP body）。
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpgradeRequest {
    pub level: Level,
    /// `true` 只预览（返回 planId 供确认）。
    pub dry_run: bool,
    /// 确认时带回预览快照，三者全等才执行。
    pub expected_head: Option<String>,
    pub expected_version: Option<String>,
    pub expected_plan: Option<String>,
}

/// 执行（或预览）一次版本升级。
///
/// `cwd` 是项目里的任意目录，内部会解析到仓库根。调用方必须已经确认这是桌面壳。
///
/// 安全顺序与 py 的 `desktop_release` 逐条对齐：
/// 1. 必须是仓库根（不是子目录）
/// 2. 必须在本地分支上（`symbolic-ref`）
/// 3. 不能有进行中的合并/变基/拣选
/// 4. 工作区必须干净（含未跟踪文件）
/// 5. 标签不能已存在
/// 6. **确认时** head/version/planId 三者全等，否则拒绝
/// 7. 落盘后复查：版本全对齐、只有预期文件变化、HEAD 未变、提交内容未被动过
pub fn upgrade(root: &Path, request: &UpgradeRequest) -> Result<Value> {
    let toplevel = git(root, &["rev-parse", "--show-toplevel"])?;
    let toplevel = PathBuf::from(toplevel);
    let root_real = root
        .canonicalize()
        .map_err(|e| BumpError(format!("无法解析仓库根目录：{e}")))?;
    let top_real = toplevel
        .canonicalize()
        .map_err(|e| BumpError(format!("无法解析 Git 顶层目录：{e}")))?;
    if top_real != root_real {
        return fail("版本升级必须在项目仓库根目录执行");
    }

    if git(&toplevel, &["symbolic-ref", "--quiet", "HEAD"]).is_err() {
        return fail("请先切换到本地分支再升级版本");
    }

    let git_dir = PathBuf::from(git(root, &["rev-parse", "--absolute-git-dir"])?);
    for marker in [
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
        "REVERT_HEAD",
        "rebase-merge",
        "rebase-apply",
        "sequencer",
    ] {
        if git_dir.join(marker).exists() {
            return fail("请先完成或中止正在进行的合并、变基或拣选操作");
        }
    }

    if !git(root, &["status", "--porcelain", "--untracked-files=all"])?.is_empty() {
        return fail("请先提交或贮藏所有未提交改动，再升级版本；不会混入其它修改");
    }

    let head = git(root, &["rev-parse", "HEAD"])?;
    let commits = release_commits(root, None)?;
    let mut plan = plan(root, request.level, &commits)?;
    plan.head = head.clone();

    // 标签不能重复。放在预览之前：既然注定失败，就别让用户白确认一次。
    if !git(root, &["tag", "--list", &plan.tag])?.is_empty() {
        return fail(format!("标签 {} 已存在，不能重复发布", plan.tag));
    }
    // 没有提交身份时 git commit 会失败，提前给可读提示。
    git(root, &["var", "GIT_AUTHOR_IDENT"]).map_err(|e| {
        BumpError(format!(
            "请先配置 Git 提交身份（user.name / user.email）：{e}"
        ))
    })?;
    git(root, &["var", "GIT_COMMITTER_IDENT"]).map_err(|e| {
        BumpError(format!(
            "请先配置 Git 提交身份（user.name / user.email）：{e}"
        ))
    })?;

    // 版本文件必须都被 git 跟踪，否则 --only 提交会少文件。
    let mut args: Vec<&str> = vec!["ls-files", "--error-unmatch", "--"];
    args.extend(plan.files.iter().map(String::as_str));
    git(root, &args)?;

    let mut result = plan.preview_json();
    if request.dry_run {
        return Ok(result);
    }

    let confirmed = (
        request.expected_head.as_deref(),
        request.expected_version.as_deref(),
        request.expected_plan.as_deref(),
    );
    if confirmed
        != (
            Some(plan.head.as_str()),
            Some(plan.version.as_str()),
            Some(plan.plan_id.as_str()),
        )
    {
        return fail("仓库或升级预览已经变化，请重新预览并确认");
    }

    // 落盘。任一失败都保留现场（与 py 一致：不自动回滚，让用户自己看）。
    for edit in &plan.edits {
        std::fs::write(&edit.path, &edit.content)
            .map_err(|e| BumpError(format!("写入 {} 失败：{e}", edit.rel)))?;
    }

    // 落盘后复查 1：所有位置的版本号都应等于新版本。
    for rel in CARGO_TOMLS {
        let text = std::fs::read_to_string(root.join(rel))
            .map_err(|e| BumpError(format!("读取 {rel} 失败：{e}")))?;
        let versions: Vec<&str> = text
            .lines()
            .filter_map(|l| l.strip_prefix("version = \""))
            .filter_map(|r| r.strip_suffix('"'))
            .collect();
        if !versions.contains(&plan.version.as_str()) {
            return fail("升级后版本号未全部对齐，已停止提交");
        }
    }
    let tauri_text = std::fs::read_to_string(root.join(TAURI_CONF))
        .map_err(|e| BumpError(format!("读取 {TAURI_CONF} 失败：{e}")))?;
    if read_tauri_version(&tauri_text)? != plan.version {
        return fail("升级后版本号未全部对齐，已停止提交");
    }

    // 落盘后复查 2：只有计划里的文件被改动。
    let changed: Vec<String> = git(root, &["diff", "--name-only", "-z", "HEAD", "--"])?
        .split('\0')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    if changed.is_empty() || changed.iter().any(|c| !plan.files.contains(c)) {
        return fail("升级期间出现预期之外的文件改动，已停止提交");
    }
    // 落盘后复查 3：HEAD 没被并发移动。
    if git(root, &["rev-parse", "HEAD"])? != plan.head {
        return fail("提交历史在升级期间发生变化，已停止提交");
    }

    // `--only` 只提交版本文件：即使外部工具并发暂存了别的文件也不会混进来。
    let files_owned: Vec<String> = plan.files.clone();
    let message = format!("chore(release): 版本号升级至 {}", plan.version);
    let mut commit_args: Vec<&str> = vec![
        "-c",
        "commit.gpgsign=false",
        "commit",
        "--only",
        "-m",
        &message,
        "--",
    ];
    commit_args.extend(files_owned.iter().map(String::as_str));
    git(root, &commit_args)?;

    let commit = git(root, &["rev-parse", "HEAD"])?;
    if git(root, &["rev-parse", "HEAD^"])? != plan.head {
        return fail("提交历史在升级期间发生变化，已停止创建标签");
    }

    // 复查 4：提交内容与预期一致（防提交钩子篡改）。
    let committed: Vec<String> = git(
        root,
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            "-z",
            "HEAD",
        ],
    )?
    .split('\0')
    .filter(|s| !s.is_empty())
    .map(str::to_string)
    .collect();
    if committed != changed
        || plan
            .edits
            .iter()
            .any(|e| std::fs::read(&e.path).ok().as_deref() != Some(e.content.as_bytes()))
        || !git(root, &["status", "--porcelain", "--untracked-files=all"])?.is_empty()
    {
        return fail("提交钩子或外部工具修改了仓库，请检查版本提交；尚未创建标签");
    }

    // 显式禁用 tag.gpgsign：普通标签不能因用户全局配置变成签名操作。
    git(
        root,
        &[
            "-c",
            "tag.gpgsign=false",
            "tag",
            "--no-sign",
            &plan.tag,
            &commit,
        ],
    )?;

    result["commit"] = json!(commit);
    Ok(result)
}
