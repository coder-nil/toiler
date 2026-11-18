//! `/api/sessions/{id}` 的补充 route（阶段 3 补齐）。
//!
//! | route | 行为 |
//! |---|---|
//! | `GET  /api/sessions/{id}/context` | 只取 context + paging（形状与 detail 的 `context` 一致） |
//! | `GET  /api/sessions/{id}/export` | 导出单文件 HTML（转写稿） |
//! | `GET  /api/sessions/{id}/entries/{entryId}/thinking?blockIndex=N` | 取某个 thinking 块的原文 |
//! | `POST /api/sessions/{id}/auto-name` | 让模型给会话起标题并写回 `session_info` |
//!
//! 与 pi 的差异（`docs/contracts/README.md` 有记录）：
//!
//! - 导出：pi 用 `pi` CLI 生成一份带树状导航的 HTML；toiler 自己渲染一份
//!   线性转写稿（同一套 header / 下载文件名规则，内容更朴素）。
//! - 导出页被展示（`?inline=1` / `?desktop=1`）时多一个常驻的「返回对话」按钮，
//!   理由见 [`back_entry_html`]。下载形态（attachment）不注入任何东西。

use std::collections::HashMap;

use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use pulldown_cmark::{Options, Parser};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::security::Trusted;
use crate::session_view;
use crate::sessions_store::{read_session, RawSession};
use crate::state::AppState;

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct ContextParams {
    #[serde(rename = "deferThinking")]
    defer_thinking: Option<String>,
    #[serde(rename = "deferMedia")]
    defer_media: Option<String>,
    #[serde(rename = "leafId")]
    leaf_id: Option<String>,
    before: Option<String>,
    limit: Option<String>,
}

/// `GET /api/sessions/{id}/context`
pub async fn context(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Query(params): Query<ContextParams>,
) -> Response {
    let Some(path) = state.sessions().path_of(&id) else {
        return error(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };

    let view = session_view::context_view(&session.entries, params.leaf_id.as_deref());
    let page = match params.before.as_deref().filter(|b| !b.is_empty()) {
        Some(before) => session_view::page_until(
            &view.messages,
            &view.entry_ids,
            before,
            params.limit.as_deref(),
        ),
        None => session_view::page_tail(&view.messages, params.limit.as_deref()),
    };
    let messages: Vec<Value> = view.messages[page.start..page.end]
        .iter()
        .map(|message| {
            // 与 `GET /api/sessions/{id}` 用同一套投影：前端翻页时也带这两个
            // 参数（`useAgentSession.ts` 的 `loadEarlierMessages`），少了它们
            // 就会「首屏的图是占位、翻上去的图是完整 base64」，同一屏两种表现。
            let message = if params.defer_thinking.is_some() {
                session_view::defer_thinking_blocks(message)
            } else {
                message.clone()
            };
            if params.defer_media.is_some() {
                session_view::defer_media_blocks(&message)
            } else {
                message
            }
        })
        .collect();
    let entry_ids: Vec<String> = view.entry_ids
        [page.start.min(view.entry_ids.len())..page.end.min(view.entry_ids.len())]
        .to_vec();

    Json(json!({
        "context": {
            "messages": messages,
            "entryIds": entry_ids,
            "thinkingLevel": view.thinking_level,
            "model": view.model,
        },
        "paging": {
            "hasEarlier": page.has_earlier(),
            "totalMessages": page.total,
            "startIndex": page.start_index(),
            "pageSize": session_view::parse_limit(
                params.limit.as_deref(),
                session_view::DEFAULT_PAGE_SIZE,
            ),
        },
    }))
    .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct ThinkingQuery {
    #[serde(rename = "blockIndex")]
    block_index: Option<String>,
}

/// `GET /api/sessions/{id}/entries/{entryId}/thinking?blockIndex=N`
pub async fn thinking(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path((id, entry_id)): Path<(String, String)>,
    Query(query): Query<ThinkingQuery>,
) -> Response {
    let Some(index) = query
        .block_index
        .as_deref()
        .and_then(|raw| raw.parse::<usize>().ok())
    else {
        return error(StatusCode::BAD_REQUEST, "Valid blockIndex is required");
    };
    let Some(path) = state.sessions().path_of(&id) else {
        return error(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };
    let Some(entry) = session
        .entries
        .iter()
        .find(|e| e.get("id").and_then(Value::as_str) == Some(entry_id.as_str()))
    else {
        return error(StatusCode::NOT_FOUND, "Assistant message not found");
    };
    let Some(message) = entry.get("message").filter(|_| {
        entry.get("type").and_then(Value::as_str) == Some("message")
            && entry
                .get("message")
                .and_then(|m| m.get("role"))
                .and_then(Value::as_str)
                == Some("assistant")
    }) else {
        return error(StatusCode::NOT_FOUND, "Assistant message not found");
    };
    let Some(block) = message
        .get("content")
        .and_then(Value::as_array)
        .and_then(|blocks| blocks.get(index))
    else {
        return error(StatusCode::NOT_FOUND, "Thinking block not found");
    };
    if block.get("type").and_then(Value::as_str) != Some("thinking") {
        return error(StatusCode::NOT_FOUND, "Thinking block not found");
    }
    Json(json!({ "thinking": block.get("thinking").cloned().unwrap_or(Value::Null) }))
        .into_response()
}

#[derive(Debug, Deserialize, Default)]
pub struct ExportParams {
    inline: Option<String>,
    desktop: Option<String>,
    #[serde(rename = "returnTo")]
    return_to: Option<String>,
    /// 界面语言（`zh-CN` / `en`），只影响导出页自己的按钮文案与 `<html lang>`。
    lang: Option<String>,
    /// `1` 表示页面跑在**覆盖式标题栏**的桌面壳里：macOS 的红绿灯浮在网页
    /// 内容上（距顶约 42px、左侧 92px 内都是它），所以左侧大纲与顶部按钮都得
    /// 往下、往右让位，否则侧栏第一项就被按钮盖住。
    #[serde(rename = "overlayChrome")]
    overlay_chrome: Option<String>,
    /// 应用里**已解析**的主题：`light` / `dark`。
    ///
    /// 导出页是独立的一份 HTML（桌面壳里还是整页导航），拿不到应用的 React 状态；
    /// 只靠 `prefers-color-scheme` 的话，「系统深色 + 应用里选浅色」会让导出页
    /// 永远黑着。缺省（`auto` 或没传）时仍由系统偏好决定。
    theme: Option<String>,
}

/// `GET /api/sessions/{id}/export` —— 下载一份自包含的 HTML。
pub async fn export(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
    Query(params): Query<ExportParams>,
) -> Response {
    let Some(path) = state.sessions().path_of(&id) else {
        return error(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };

    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| id.clone());
    let filename = format!("pi-session-{stem}.html");
    let ascii = sanitize_ascii_filename(&filename);
    let disposition = format!(
        "{}; filename=\"{}\"; filename*=UTF-8''{}",
        if params.inline.as_deref() == Some("1") {
            "inline"
        } else {
            "attachment"
        },
        ascii,
        percent_encode(&filename)
    );

    let chinese = is_chinese(params.lang.as_deref());
    let overlay_chrome = params.overlay_chrome.as_deref() == Some("1");
    let theme = parse_theme(params.theme.as_deref());
    let mut html = render_export_html(
        &session,
        &id,
        if chinese { "zh-CN" } else { "en" },
        overlay_chrome,
        theme,
    );

    // 页面被浏览器展示时（而不是下载）才注入返回入口。
    if params.inline.as_deref() == Some("1") || params.desktop.as_deref() == Some("1") {
        let target = params
            .return_to
            .as_deref()
            .and_then(allowed_return_to)
            .map(|url| format!("location.replace({})", js_string_literal(&url)))
            // 没有可用的回跳地址：有历史就后退，没有就关掉这个标签页。
            .unwrap_or_else(|| {
                "if(history.length>1){history.back()}else{window.close()}".to_string()
            });
        let label = if chinese {
            STRINGS_ZH.back
        } else {
            STRINGS_EN.back
        };
        // 结构与脚本进 `</body>`：把 `<div>` 塞进 head 不是合法 HTML（浏览器会把
        // 后面的节点挤出 head）。样式已经在 `EXPORT_STYLE` 里，这里不再重复。
        let strings = strings_for(chinese);
        if let Some(pos) = html.rfind("</body>") {
            html.insert_str(
                pos,
                &back_entry_html(label, &target, strings.esc_hint, strings.esc_dismiss),
            );
        }
    }

    let mut response = Response::new(axum::body::Body::from(html));
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/html; charset=utf-8"),
    );
    if let Ok(value) = axum::http::HeaderValue::from_str(&disposition) {
        headers.insert(header::CONTENT_DISPOSITION, value);
    }
    headers.insert(
        header::CACHE_CONTROL,
        axum::http::HeaderValue::from_static("no-cache"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        axum::http::HeaderValue::from_static("frame-ancestors 'none'"),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        axum::http::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::X_FRAME_OPTIONS,
        axum::http::HeaderValue::from_static("DENY"),
    );
    response
}

/// 把一段文本变成可安全内嵌进 `<script>` 的 JS 字符串字面量。
///
/// `json!` 负责引号与反斜杠转义；`</` 再拆开一次，避免 `</script>` 提前闭合脚本块
/// （`allowed_return_to` 走的是 URL 解析，正常情况已经编码成 `%3C`，这里是兜底）。
fn js_string_literal(text: &str) -> String {
    json!(text).to_string().replace("</", "<\\/")
}

fn is_chinese(lang: Option<&str>) -> bool {
    lang.map(|value| value.to_ascii_lowercase().starts_with("zh"))
        .unwrap_or(false)
}

/// 导出页的显式主题。`None` 表示跟随系统（`prefers-color-scheme`）。
#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportTheme {
    Light,
    Dark,
}

impl ExportTheme {
    /// `<html>` 上的类名。`None` 时不挂类，完全交给 media query。
    fn root_class(self) -> &'static str {
        match self {
            ExportTheme::Light => "theme-light",
            ExportTheme::Dark => "theme-dark",
        }
    }

    /// 用于 `<meta name="color-scheme">`：让滚动条、表单控件这些浏览器原生部件
    /// 也跟着走，否则浅色页面里会出现一条黑滚动条。
    fn color_scheme(self) -> &'static str {
        match self {
            ExportTheme::Light => "light",
            ExportTheme::Dark => "dark",
        }
    }
}

fn parse_theme(raw: Option<&str>) -> Option<ExportTheme> {
    match raw?.trim().to_ascii_lowercase().as_str() {
        "light" => Some(ExportTheme::Light),
        "dark" => Some(ExportTheme::Dark),
        // `auto` / 空 / 认不出的值：交给系统偏好。
        _ => None,
    }
}

/// 导出页的一份文案表（只有中英两种，跟界面语言走）。
#[derive(Clone, Copy)]
struct Strings {
    lang: &'static str,
    back: &'static str,
    user: &'static str,
    thinking: &'static str,
    tool_error: &'static str,
    arguments: &'static str,
    result: &'static str,
    result_missing: &'static str,
    result_lines: &'static str,
    output_empty: &'static str,
    truncated: &'static str,
    outline: &'static str,
    round: &'static str,
    preamble: &'static str,
    messages: &'static str,
    no_messages: &'static str,
    custom: &'static str,
    image: &'static str,
    system_prompt: &'static str,
    system_empty: &'static str,
    system_chars: &'static str,
    system_versions: &'static str,
    esc_hint: &'static str,
    esc_dismiss: &'static str,
    section_preamble: &'static str,
    section_tools: &'static str,
    section_rules: &'static str,
    section_docs: &'static str,
    section_project_context: &'static str,
    section_project_instructions: &'static str,
    section_skills: &'static str,
    section_cwd: &'static str,
    section_env: &'static str,
    section_other: &'static str,
}

const STRINGS_ZH: Strings = Strings {
    lang: "zh-CN",
    back: "返回对话",
    user: "提问",
    thinking: "思考",
    tool_error: "失败",
    arguments: "参数",
    result: "结果",
    result_missing: "没有记录到结果",
    result_lines: "{n} 行",
    output_empty: "（无输出）",
    truncated: "\n\n··· （已截断：只保留前 {kept} 字，原文共 {total} 字）",
    outline: "会话目录",
    round: "第 {n} 轮",
    preamble: "会话开头（还没有提问）",
    messages: "条消息",
    no_messages: "这个会话还没有任何消息。",
    custom: "自定义消息",
    image: "[图片]",
    system_prompt: "系统提示词",
    system_empty: "这次会话没有记录系统提示词。",
    system_chars: "{count} 字符",
    system_versions: "本会话记了 {count} 次（已按最后一次合并）",
    esc_hint: "返回对话",
    esc_dismiss: "知道了",
    section_preamble: "角色",
    section_tools: "可用工具",
    section_rules: "行为准则",
    section_docs: "文档",
    section_project_context: "项目上下文",
    section_project_instructions: "项目约定",
    section_skills: "技能",
    section_cwd: "工作目录",
    section_env: "环境",
    section_other: "其他",
};

const STRINGS_EN: Strings = Strings {
    lang: "en",
    back: "Back to chat",
    user: "Prompt",
    thinking: "Thinking",
    tool_error: "failed",
    arguments: "Arguments",
    result: "Result",
    result_missing: "no result recorded",
    result_lines: "{n} lines",
    output_empty: "(no output)",
    truncated: "\n\n··· (truncated: kept first {kept} of {total} chars)",
    outline: "Contents",
    round: "Turn {n}",
    preamble: "Session start (no prompt yet)",
    messages: "messages",
    no_messages: "This session has no messages yet.",
    custom: "Custom message",
    image: "[image]",
    system_prompt: "System prompt",
    system_empty: "This session did not record a system prompt.",
    system_chars: "{count} chars",
    system_versions: "recorded {count}× in this session (merged)",
    esc_hint: "to go back",
    esc_dismiss: "Got it",
    section_preamble: "Role",
    section_tools: "Tools",
    section_rules: "Guidelines",
    section_docs: "Docs",
    section_project_context: "Project context",
    section_project_instructions: "Project rules",
    section_skills: "Skills",
    section_cwd: "Working directory",
    section_env: "Environment",
    section_other: "Other",
};

fn strings_for(chinese: bool) -> Strings {
    if chinese {
        STRINGS_ZH
    } else {
        STRINGS_EN
    }
}

/// 导出页注入的脚本（按钮点击 / Esc 返回 + Esc 提示 toast）。
///
/// **不要改成带行尾 `\` 的 Rust 字符串。** Rust 里行末的 `\` 会连同后面的换行
/// 一起吃掉，整个脚本会被压成一行；只要里面有 `//` 单行注释，它就会把同行的
/// 后续代码和最后的 `}})` 全部注释掉 → 语法错误 → **整段脚本不执行**（返回按钮
/// 和 Esc 一起失效，就是曾经出现过的症状）。这里用真正的多行字符串，占位符用
/// `@@NAME@@` 在最后做 `replace`，既不碰 `{{}}` 转义，也不依赖换行保留。
const BACK_ENTRY_SCRIPT: &str = r#"
(function () {
  var button = document.getElementById("toiler-export-back");
  if (!button) return;

  var go = function () { @@TARGET@@ };
  button.addEventListener("click", go);
  document.addEventListener("keydown", function (event) {
    if (event.key === "Escape") go();
  });

  /* Esc 提示：进页面就浮出，点掉 / 到点自动收起。用 sessionStorage 记住本次
     停留已提示过，切回聊天再进来不会每次都弹。 */
  var toast = document.getElementById("toiler-esc-hint");
  var close = document.getElementById("toiler-esc-close");
  if (!toast) return;

  var KEY = "toiler-esc-hint-shown";
  var shown = false;
  try { shown = sessionStorage.getItem(KEY) === "1"; } catch (e) {}

  var drop = function () {
    if (toast && toast.parentNode) toast.parentNode.removeChild(toast);
  };
  var hide = function () {
    if (!toast) return;
    toast.classList.add("is-out");
    setTimeout(drop, 260);
    try { sessionStorage.setItem(KEY, "1"); } catch (e) {}
  };

  if (shown) {
    drop();
    return;
  }
  setTimeout(hide, 6000);
  if (close) {
    close.addEventListener("click", function (event) {
      event.stopPropagation();
      hide();
    });
  }
})();
"#;

/// 导出页的返回入口 + 「按 Esc 返回」的 toast 提示。
///
/// 导出页是**整页导航**进来的：桌面壳里 `window.open` 不建子窗口，前端直接把
/// WebView 指向本页（见 `AppShell.handleViewFullHistory`），页面本身就是最终位置，
/// 没有浏览器后退键可按，所以必须自带一个看得见的返回按钮。
///
/// Esc 也能返回，但这个手势**没有任何可见线索**——不提示就等于不存在。
/// 提示做成页面底部的 toast（而不是挤在按钮旁边的小条）：它是一次性告知，
/// 不该跟常驻控件争注意力。背景/文字用主题变量，深色浅色自动跟随。
fn back_entry_html(label: &str, target: &str, hint: &str, dismiss: &str) -> String {
    let label = escape_html(label);
    format!(
        r#"<div class="toiler-back">
<button type="button" id="toiler-export-back" aria-label="{label}" title="{label}">
<svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true"><path d="M15 18l-6-6 6-6"/></svg>
<span>{label}</span></button></div>
<div class="toiler-toast" id="toiler-esc-hint" role="status" aria-live="polite">
<kbd>Esc</kbd><span>{hint}</span>
<button type="button" class="toiler-toast-close" id="toiler-esc-close" aria-label="{dismiss}" title="{dismiss}">×</button></div>
<script>{script}</script>"#,
        hint = escape_html(hint),
        dismiss = escape_html(dismiss),
        script = BACK_ENTRY_SCRIPT.replace("@@TARGET@@", target),
    )
}

/// 只允许 tauri / 127.0.0.1 的 returnTo（pi 的同名白名单）。
fn allowed_return_to(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    let ok = (url.scheme() == "tauri" && url.host_str() == Some("localhost"))
        || (url.scheme() == "http" && url.host_str() == Some("127.0.0.1"));
    ok.then(|| url.to_string())
}

fn sanitize_ascii_filename(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_graphic() && !matches!(c, '"' | '\\' | ';') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if cleaned.trim_matches('_').is_empty() {
        "session.html".to_string()
    } else {
        cleaned
    }
}

fn percent_encode(name: &str) -> String {
    let mut out = String::new();
    for byte in name.as_bytes() {
        let ch = *byte as char;
        let unreserved = ch.is_ascii_alphanumeric() || matches!(ch, '-' | '.' | '_' | '~');
        if unreserved {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// 深色主题的变量值（**单一来源**）。
///
/// 两处引用它们：`@media (prefers-color-scheme:dark)`（跟随系统）与
/// `html.theme-dark`（用户在应用里手动选了深色）。两份各自写一份会慢慢跑偏，
/// 所以这里只有一份。
const DARK_VARS: &str = "--bg:#0a0a0b;--panel:#141416;--sidebar:#0e0e10;\
--ink:#f2f2f3;--muted:#a0a0a8;--dim:#6e6e78;--line:#232327;--line-soft:#1b1b1f;\
--soft:#1a1a1e;--user:#141c33;--user-line:#1f2c4d;--tool:#241d0b;--tool-line:#3d3218;\
--accent:#8b92f8;--accent-soft:#1c1d33;--code:#1b1b1f;--err:#f2706f;--ok:#4ade80;\
--shadow:0 1px 2px rgba(0,0,0,.5),0 1px 3px rgba(0,0,0,.35);\
/* toast：深色主题下反而要用亮一点的浮层才能看出边界 */\
--toast-bg:#2a2a31;--toast-ink:#f4f4f6;--toast-line:#3d3d47;\
--toast-kbd-bg:#22222a;--toast-close:#9a9aa6;--toast-hover:#35353f";

/// 导出页的样式表。
///
/// 设计目标只有一个：**扫读**。手段：轮次之间用分隔线与左侧色条断开；
/// 工具调用与思考降成一行可以折起的细窄条（正文 76% 的体积都是工具输出，
/// 平铺就会把主线淹掉）；正文宽度锁在 820px 的舒适阅读区间。
///
/// 深浅色有三种状态，优先级从低到高：
/// 1. 默认（`:root` 里的浅色变量）；
/// 2. 系统深色（`@media (prefers-color-scheme:dark)`）；
/// 3. 用户在应用里显式选的主题（`html.theme-dark` / `html.theme-light`）。
///
/// 第 3 层必须是**类选择器**而不是 media query：导出页是另一个页面（甚至另一个
/// WebView 导航），拿不到应用的 React 状态，只能靠前端透传 `?theme=`。没有它时
/// 用户「系统深色 + 应用里选浅色」的组合下，导出页永远黑着——这就是"浅色主题
/// 不生效"的真因。
fn export_style() -> &'static str {
    static STYLE: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    STYLE.get_or_init(|| {
        // 2) 跟随系统，3) 显式深色。两条规则内容一样，只是命中条件不同。
        // 显式主题那条必须存在：类选择器 `html.theme-dark` 权重 (0,1,1)
        // 高于 media 里的 `:root` (0,1,0)，才能在系统偏好之上生效。
        let dark_rules = format!(
            "@media (prefers-color-scheme:dark){{:root{{{DARK_VARS}}}}}\n\
             html.theme-dark{{{DARK_VARS}}}"
        );
        format!(
            // 浅色：默认值。显式浅色（theme-light）不需要覆盖任何东西，
            // 只要保证不被 media query 拖走即可（见 EXPORT_STYLE_REST 里那条重置）。
            r#":root{{--bg:#fbfbfa;--panel:#fff;--sidebar:#f6f6f5;--ink:#1c1c1f;--muted:#6b6b73;
--dim:#9a9aa2;--line:#e8e8e6;--line-soft:#f0f0ee;--soft:#f4f4f3;--user:#f0f5ff;
--user-line:#d3e0fb;--tool:#fdfaef;--tool-line:#f0e3b8;--accent:#4f46e5;
--accent-soft:#eef0fe;--code:#f5f5f4;--err:#d93a3a;--ok:#16803c;
--shadow:0 1px 2px rgba(16,16,20,.05),0 1px 3px rgba(16,16,20,.04);
/* toast：浅色主题下用深底浮层，和页面拉开层次 */
--toast-bg:#1f1f25;--toast-ink:#f6f6f8;--toast-line:#33333d;
--toast-kbd-bg:#2c2c34;--toast-close:#9a9aa6;--toast-hover:#2f2f38}}
{dark_rules}{rest}"#,
            rest = EXPORT_STYLE_REST,
        )
    })
}

/// `export_style()` 里不随主题变化的那部分（比着分开写更好改样式）。
const EXPORT_STYLE_REST: &str = r#"
/* 显式浅色：即使系统是深色也必须回到浅色变量。
   这里重复一遍浅色值，是因为 `@media` 那条规则的选择器权重与 `:root` 相同
   但位置在后，光靠 `:root` 拦不住——必须用同权重的 `html.theme-light` 覆盖。 */
html.theme-light{--bg:#fbfbfa;--panel:#fff;--sidebar:#f6f6f5;--ink:#1c1c1f;--muted:#6b6b73;
--dim:#9a9aa2;--line:#e8e8e6;--line-soft:#f0f0ee;--soft:#f4f4f3;--user:#f0f5ff;
--user-line:#d3e0fb;--tool:#fdfaef;--tool-line:#f0e3b8;--accent:#4f46e5;
--accent-soft:#eef0fe;--code:#f5f5f4;--err:#d93a3a;--ok:#16803c;
--shadow:0 1px 2px rgba(16,16,20,.05),0 1px 3px rgba(16,16,20,.04);
--toast-bg:#1f1f25;--toast-ink:#f6f6f8;--toast-line:#33333d;
--toast-kbd-bg:#2c2c34;--toast-close:#9a9aa6;--toast-hover:#2f2f38}
*{box-sizing:border-box}
html{scroll-behavior:smooth;scroll-padding-top:24px}
body{margin:0;background:var(--bg);color:var(--ink);
font:15.5px/1.7 -apple-system,BlinkMacSystemFont,'Segoe UI','PingFang SC',
'Hiragino Sans GB','Microsoft YaHei',sans-serif;-webkit-font-smoothing:antialiased;
text-rendering:optimizeLegibility}
.shell{display:flex;align-items:flex-start;min-height:100vh}

/* ---- 返回按钮 + 「按 Esc 返回」提示（fixed，浮在右上） ---- */
.toiler-back{position:fixed;top:14px;right:16px;z-index:20;display:flex;
align-items:center;gap:10px}
.toiler-back>button{display:flex;align-items:center;gap:6px;height:32px;padding:0 12px;
font:13px/1 inherit;color:var(--ink);background:var(--panel);border:1px solid var(--line);
border-radius:999px;box-shadow:0 1px 3px rgba(0,0,0,.08);cursor:pointer;flex:none;
transition:border-color .12s,background .12s}
.toiler-back>button:hover{border-color:var(--dim);background:var(--soft)}
.toiler-back>button:focus-visible{outline:2px solid var(--accent);outline-offset:2px}
/* ---- Esc 提示：底部居中的 toast，背景/文字跟主题走 ---- */
.toiler-toast{position:fixed;left:50%;bottom:28px;z-index:60;
display:flex;align-items:center;gap:10px;padding:9px 8px 9px 14px;
font-size:13px;line-height:1.4;color:var(--toast-ink);background:var(--toast-bg);
border:1px solid var(--toast-line);border-radius:999px;
box-shadow:0 6px 20px rgba(12,12,18,.16),0 2px 6px rgba(12,12,18,.1);
white-space:nowrap;transform:translate(-50%,0);
animation:toiler-toast-in .26s ease-out both;transition:opacity .24s ease,transform .24s ease}
.toiler-toast.is-out{opacity:0;transform:translate(-50%,8px);pointer-events:none}
.toiler-toast kbd{font:11px/1 ui-monospace,SFMono-Regular,Menlo,monospace;
background:var(--toast-kbd-bg);border:1px solid var(--toast-line);
border-bottom-width:2px;border-radius:5px;padding:3px 6px;color:var(--toast-ink)}
.toiler-toast-close{width:20px;height:20px;flex:none;display:flex;align-items:center;
justify-content:center;padding:0;font-size:15px;line-height:1;color:var(--toast-close);
background:none;border:none;border-radius:50%;cursor:pointer}
.toiler-toast-close:hover{color:var(--toast-ink);background:var(--toast-hover)}
@keyframes toiler-toast-in{from{opacity:0;transform:translate(-50%,12px)}
to{opacity:1;transform:translate(-50%,0)}}
@media (prefers-reduced-motion:reduce){.toiler-toast{animation:none;transition:none}}

/* ---- 左侧大纲：常驻，占 260px，窄屏自动收起 ---- */
.outline{position:sticky;top:0;flex:0 0 260px;width:260px;height:100vh;
background:var(--sidebar);border-right:1px solid var(--line);overflow:hidden}
.outline-inner{height:100%;overflow-y:auto;padding:20px 12px 32px;
scrollbar-width:thin}
.outline-head{display:flex;align-items:center;gap:8px;margin:0 0 12px;padding:0 10px;
font-size:11px;font-weight:600;text-transform:uppercase;letter-spacing:.09em;
color:var(--dim)}
.outline-count{margin-left:auto;font-weight:500;letter-spacing:0;
font-variant-numeric:tabular-nums;color:var(--dim);opacity:.8}
.toc-list{display:flex;flex-direction:column;gap:1px}
.toc-item{display:block;padding:7px 10px;border-radius:7px;text-decoration:none;
color:var(--muted);border-left:2px solid transparent;transition:background .12s,color .12s}
.toc-item:hover{background:var(--soft);color:var(--ink)}
.toc-item:focus-visible{outline:2px solid var(--accent);outline-offset:-1px}
.toc-title{display:block;font-size:13px;line-height:1.45;
overflow:hidden;display:-webkit-box;-webkit-line-clamp:2;-webkit-box-orient:vertical}
.toc-meta{display:block;margin-top:2px;font-size:11px;color:var(--dim);
font-variant-numeric:tabular-nums}
.toc-system{color:var(--ink);font-weight:600}
.toc-system .toc-title{color:var(--accent)}
.toc-system+.toc-item{margin-top:8px}
.toc-system+.toc-item::before{content:'';display:block;margin:-4px 0 6px;
border-top:1px solid var(--line)}

/* ---- 主列 ---- */
.column{flex:1;min-width:0}
header{max-width:780px;margin:0 auto;padding:44px 32px 20px}
header h1{font-size:23px;line-height:1.35;margin:0 0 6px;letter-spacing:-.015em;
font-weight:650}
.meta{color:var(--muted);font-size:12.5px;margin:0;font-variant-numeric:tabular-nums}
main{max-width:780px;margin:0 auto;padding:0 32px 120px}

/* ---- 轮次 ---- */
.turn{padding-top:8px}
.turn+.turn{border-top:1px solid var(--line);margin-top:32px;padding-top:28px}
.system-block{border-top:1px solid var(--line);padding-top:28px;margin-bottom:4px}
.turn-head{font-size:15px;font-weight:600;margin:0 0 16px;padding-left:12px;
border-left:3px solid var(--accent);line-height:1.5;color:var(--ink)}

/* ---- 消息 ---- */
.msg{border-radius:11px;margin:0 0 10px}
.msg.user{background:var(--user);border:1px solid var(--user-line);
box-shadow:var(--shadow)}
.msg.assistant{padding-top:2px}
.msg.custom{background:var(--soft);border:1px solid var(--line);box-shadow:var(--shadow)}
.head{display:flex;align-items:center;gap:10px;padding:10px 16px 0;font-size:10.5px;
text-transform:uppercase;letter-spacing:.09em;color:var(--dim)}
.head .role{font-weight:700;color:var(--muted)}
.head.quiet{padding:0 4px 2px;margin-top:14px}
.head time{font-variant-numeric:tabular-nums;letter-spacing:0;color:var(--dim)}
.main{padding:8px 16px 14px}
.msg.assistant>.main{padding:0 4px 2px}
.msg.assistant>.aside{padding-left:14px;border-left:1px solid var(--line-soft);
margin:10px 0 4px 4px}
.msg.assistant>.aside:empty{display:none;border:none}
.aside:empty{display:none}

/* ---- markdown ---- */
.main>:first-child{margin-top:0}
.main>:last-child{margin-bottom:0}
.main p{margin:0 0 11px}
.main ul,.main ol{margin:0 0 11px;padding-left:24px}
.main li{margin:3px 0}
.main li::marker{color:var(--dim)}
.main h1,.main h2,.main h3,.main h4,.main h5,.main h6{line-height:1.4;
margin:22px 0 10px;font-weight:650;letter-spacing:-.01em}
.main h1{font-size:19px}.main h2{font-size:16.5px}.main h3{font-size:15px}
.main h4,.main h5,.main h6{font-size:14px;color:var(--muted)}
.main a{color:var(--accent);text-decoration:none;border-bottom:1px solid var(--accent-soft)}
.main a:hover{border-bottom-color:var(--accent)}
.main strong{font-weight:650}
.main code{font:12.5px ui-monospace,SFMono-Regular,'SF Mono',Menlo,monospace;
background:var(--code);padding:1.5px 5px;border-radius:5px;
border:1px solid var(--line-soft)}
.main pre{background:var(--code);padding:14px 16px;border-radius:10px;overflow:auto;
margin:0 0 12px;font-size:12.5px;line-height:1.6;border:1px solid var(--line-soft)}
.main pre code{background:none;padding:0;border:none;font-size:inherit}
.main table{border-collapse:collapse;margin:0 0 12px;font-size:13.5px;display:block;
overflow:auto;border-radius:8px}
.main th,.main td{border:1px solid var(--line);padding:7px 12px;text-align:left}
.main th{background:var(--soft);font-weight:600}
.main blockquote{margin:0 0 12px;padding:2px 16px;border-left:3px solid var(--line);
color:var(--muted)}
.main h1,.main h2,.main h3{border:none}
.main hr{border:none;border-top:1px solid var(--line);margin:22px 0}
.main img{max-width:100%;border-radius:8px}
.media{color:var(--muted);font-size:13px}

/* ---- 折叠块：工具 / 思考 / 提示词分节 ---- */
details{margin:5px 0;border-radius:8px}
details summary{cursor:pointer;display:flex;align-items:baseline;gap:8px;
padding:5px 10px;font-size:12.5px;color:var(--muted);border-radius:8px;
list-style:none;user-select:none;transition:background .1s,color .1s}
details summary::-webkit-details-marker{display:none}
details summary:hover{background:var(--soft);color:var(--ink)}
details[open]>summary{border-radius:8px 8px 0 0}
summary .badge{flex:none;min-width:14px;text-align:center;opacity:.45;
font-size:11px;line-height:1;font-variant-numeric:tabular-nums}
summary .tool-name{flex:none;font-weight:600;color:var(--ink);font-size:12px;
font-family:ui-monospace,SFMono-Regular,Menlo,monospace}
summary .tool-hint{flex:1;min-width:0;overflow:hidden;text-overflow:ellipsis;
white-space:nowrap;opacity:.7;font-family:ui-monospace,SFMono-Regular,Menlo,monospace;
font-size:11.5px}
summary .tool-error{flex:none;color:var(--err);font-weight:700}
/* 工具条不用实心底色：真实会话一轮里几十条，黄块排队会把主线又一次淹掉。
   只留一条极淡的左描边与浅底，展开时才显色。 */
details.tool{background:transparent;border:none;border-left:2px solid var(--line-soft);
border-radius:0 7px 7px 0;padding-left:2px}
details.tool[open]{background:var(--soft);border-left-color:var(--tool-line)}
details.tool.is-error{border-left-color:var(--err)}
/* 摘要右侧的「N 行」：告诉你这个块里有多少输出，不用展开就知道该不该点。 */
summary .tool-count{flex:none;color:var(--dim);font-size:11px;
font-variant-numeric:tabular-nums}
/* 工具块展开后分「参数 / 结果」两段：每段一个小区块标题 + 代码块，
   两段之间用一条极淡分隔线断开，避免参数和输出糊成一片。 */
.tool-part{padding:0 0 2px}
.tool-part+.tool-part{border-top:1px solid var(--line-soft)
}
.tool-part h4{margin:0;padding:8px 14px 0;font-size:10.5px;font-weight:700;
text-transform:uppercase;letter-spacing:.09em;color:var(--dim)}
.tool-part pre{border-radius:0 0 7px 7px}
.tool-part.is-missing p{margin:0;padding:5px 14px 10px;font-size:12px;
color:var(--dim);font-style:italic}
details.prompt-section{background:var(--panel);border:1px solid var(--line)}
details.prompt-section{box-shadow:var(--shadow);margin:8px 0}
details.prompt-section[open]{border-color:var(--accent)}
details.thinking{background:transparent;border:none}
details.thinking>summary{color:var(--dim)}
details.thinking>summary:hover{color:var(--muted)}
details pre{margin:0;padding:11px 14px;background:var(--code);overflow:auto;
font:12.5px/1.6 ui-monospace,SFMono-Regular,'SF Mono',Menlo,monospace;
white-space:pre-wrap;overflow-wrap:anywhere;border-radius:0 0 8px 8px;
max-height:420px}
details.thinking pre{background:transparent;color:var(--muted)}
.empty{color:var(--muted)}
.prompt-meta{color:var(--dim);font-size:12px;margin:0 0 10px;
font-variant-numeric:tabular-nums}
:target{scroll-margin-top:20px}
:target>.turn-head{color:var(--accent)}

/* ---- 覆盖式标题栏（macOS 桌面壳）：红绿灯浮在网页内容上 ----
   系统标题栏高约 42px，三个按钮从窗口左侧 20px 起、共占约 76px 宽。
   侧栏从 y=0 开始排的话，第一项正好压在按钮底下（实测「会话目录」被盖住），
   所以侧栏内容往下让 48px，header 与返回按钮同步下移。 */
.overlay-chrome .outline-inner{padding-top:50px}
.overlay-chrome header{padding-top:58px}
.overlay-chrome .toiler-back{top:46px}

/* ---- 窄屏：大纲折到顶部，横向滚动列表 ---- */
@media (max-width:1000px){
.shell{display:block}
.outline{position:sticky;top:0;width:auto;height:auto;max-height:none;
border-right:none;border-bottom:1px solid var(--line);z-index:12}
.outline-inner{display:flex;gap:8px;align-items:center;overflow-x:auto;
overflow-y:hidden;padding:8px 16px;scrollbar-width:none}
.overlay-chrome .outline-inner{padding:40px 16px 8px}
.overlay-chrome header{padding-top:52px}
.overlay-chrome .toiler-back{top:42px}
.outline-inner::-webkit-scrollbar{display:none}
.outline-head{flex:none;margin:0;padding:0}
.outline-count{display:none}
.toc-list{flex-direction:row;flex:none;gap:6px}
.toc-item{white-space:nowrap;padding:5px 10px;border:none;border-radius:999px;
background:var(--soft);max-width:260px}
.toc-title{-webkit-line-clamp:1;font-size:12.5px}
.toc-meta{display:none}
.toc-system+.toc-item{margin-top:0}
.toc-system+.toc-item::before{display:none}
header{padding:26px 20px 16px}
main{padding:0 20px 90px}
}
"#;

/// 一条消息在导出页上的归属类别。
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    User,
    Assistant,
    Tool,
    Custom,
}

fn kind_of(message: &Value) -> Kind {
    match message.get("role").and_then(Value::as_str) {
        Some("user") => Kind::User,
        Some("assistant") => Kind::Assistant,
        Some("toolResult") => Kind::Tool,
        _ => Kind::Custom,
    }
}

/// 从 content 里拼出纯文本（不带任何 HTML）。
fn blocks_plain_text(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    let mut out = String::new();
    for block in blocks {
        if let Some(text) = block.get("text").and_then(Value::as_str) {
            out.push_str(text);
        }
    }
    out
}

/// 渲染一段 markdown。
///
/// 导出页原先把所有正文 `escape_html` 后直接塞进 `<p>`：`\n\n` 被 HTML 折成空格，
/// 列表/标题/代码围栏全部原样吐成纯文本（真实会话里 170 段中 77 段带行内 code、
/// 9 段带列表或标题、6 段带围栏），这是「乱」的主要来源之一。
fn render_markdown(text: &str) -> String {
    if text.trim().is_empty() {
        return String::new();
    }
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    let parser = Parser::new_ext(text, options);
    // 只要段内的 HTML（不包一棵假文档树）：`push_html` 需要 `&mut String`。
    let mut out = String::new();
    pulldown_cmark::html::push_html(&mut out, parser);
    out
}

/// 一条工具调用的名字与参数。
///
/// `toolName`/`input` 是 `normalize_tool_calls` 归一化后的形状；`name`/`arguments`
/// 是 v3 文件里的原始形状。两个都认——之前只读后者，于是每个工具块都渲染成
/// 「🔧 tool」+ `<pre>null</pre>`（真实会话 157 处）。
fn tool_name_and_args(block: &Value) -> (String, Option<Value>) {
    let name = block
        .get("toolName")
        .or_else(|| block.get("name"))
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("tool")
        .to_string();
    let args = block
        .get("input")
        .or_else(|| block.get("arguments"))
        .filter(|value| !value.is_null())
        .cloned();
    (name, args)
}

/// 把一个参数对象压成一行摘要：优先取最有信息量的字段。
fn args_one_line(args: Option<&Value>) -> String {
    let Some(args) = args else {
        return String::new();
    };
    if let Some(text) = args.as_str() {
        return text.lines().next().unwrap_or_default().to_string();
    }
    let Some(map) = args.as_object() else {
        return String::new();
    };
    for key in [
        "command",
        "filePath",
        "path",
        "pattern",
        "query",
        "description",
        "url",
    ] {
        if let Some(value) = map.get(key).and_then(Value::as_str) {
            if !value.trim().is_empty() {
                return value.lines().next().unwrap_or_default().to_string();
            }
        }
    }
    // 没命中已知字段：用第一个标量值兜底，总比空着强。
    map.values()
        .find_map(Value::as_str)
        .map(str::to_string)
        .unwrap_or_default()
}

fn truncate_chars(text: &str, max: usize) -> String {
    let mut out: String = text.chars().take(max).collect();
    if text.chars().count() > max {
        out.push('…');
    }
    out
}

/// 单个折叠块正文的字符上限。
///
/// 真实会话里单条工具输出最长 62 KB（总共 653 KB 在 `<pre>` 里）。折叠只是
/// 视觉上收起，DOM 依旧那么肥，页面照样卡；超过这个长度的部分截掉并告知。
const BLOCK_CHAR_LIMIT: usize = 4000;

/// 超长时截断并附一行提示（中英文都走这里）。
fn clamp_block(text: &str, s: Strings) -> String {
    let total = text.chars().count();
    if total <= BLOCK_CHAR_LIMIT {
        return text.to_string();
    }
    let kept = truncate_chars(text, BLOCK_CHAR_LIMIT);
    format!(
        "{kept}{notice}",
        notice = s
            .truncated
            .replace("{kept}", &BLOCK_CHAR_LIMIT.to_string())
            .replace("{total}", &total.to_string())
    )
}

/// 条目时间 → `HH:MM:SS`（本地时段不重要，用 UTC 稳定可对票）。
fn clock_of(message: &Value) -> Option<String> {
    let ms = message.get("timestamp").and_then(Value::as_i64)?;
    let iso = toiler_session::ms_to_iso(ms);
    let time = iso.split_once('T')?.1;
    Some(time.trim_end_matches('Z')[..8].to_string())
}

/// 一个可折叠块的统一形状（工具调用 / 工具输出 / 思考）。
fn details_block(class: &str, summary: &str, body: &str, open: bool) -> String {
    format!(
        "<details class=\"{class}\"{open}><summary>{summary}</summary>{body}</details>",
        open = if open { " open" } else { "" },
    )
}

fn pre(text: &str) -> String {
    format!("<pre>{}</pre>", escape_html(text))
}

/// 把一条消息渲染成「正文 + 折叠区」两段。
///
/// 正文（`main`）进主线，工具与思考进 `aside`（默认折起）。
fn render_message(
    message: &Value,
    results: &HashMap<String, ToolOutcome>,
    s: Strings,
) -> (String, String) {
    let content = message.get("content");
    let kind = kind_of(message);
    let mut main = String::new();
    let mut aside = String::new();

    // `toolResult` 的内容已经被对应的 toolCall 块接管（见 `tool_call_block`），
    // 这里不能再渲染一遍，否则一次调用会出现「调用 + 结果」两条重复的折叠条。
    if kind == Kind::Tool {
        return (String::new(), String::new());
    }

    // 字符串形态的 content（compaction 摘要等）。
    if let Some(text) = content.and_then(Value::as_str) {
        return (render_markdown(text), String::new());
    }

    let blocks: &[Value] = content
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or(&[]);

    for block in blocks {
        match block.get("type").and_then(Value::as_str) {
            Some("text") => {
                let text = block.get("text").and_then(Value::as_str).unwrap_or("");
                // toolResult 的 content 也是 `text` 块。不在这拦一下，工具输出
                // 会被当成正文渲染进主线——实测 338 KB 的 `main` 里 300+ KB
                // 都是被当成正文的工具输出，页面依旧是一锅粥。
                if kind == Kind::Tool {
                    continue;
                }
                main.push_str(&render_markdown(text));
            }
            Some("thinking") => {
                let text = block.get("thinking").and_then(Value::as_str).unwrap_or("");
                if !text.trim().is_empty() {
                    aside.push_str(&details_block(
                        "thinking",
                        s.thinking,
                        &pre(&clamp_block(text, s)),
                        false,
                    ));
                }
            }
            Some("toolCall") => {
                if let Some(block_html) = tool_call_block(block, results, s) {
                    aside.push_str(&block_html);
                }
            }
            Some("image") => {
                main.push_str(&format!("<p class=\"media\">{}</p>", escape_html(s.image)));
            }
            _ => {}
        }
    }

    (main, aside)
}

/// 一条工具调用结果（按 `toolCallId` 与调用配对后剩下的部分）。
struct ToolOutcome {
    text: String,
    failed: bool,
}

/// 按 `toolCall.id` 收集所有工具结果。
///
/// v3 文件里**调用与结果是两条独立的消息**：调用在 assistant 的
/// `content[].toolCall` 里，结果是一条单独的 `role:"toolResult"` 消息，
/// 二者只靠 `toolCall.id` ↔ `toolResult.toolCallId` 关联。原先两边各自渲染
/// 成一条折叠条、靠相邻摆放暗示关系，一旦中间夹了别的消息关系就断了。
/// 现在先把结果收成表，渲染调用时一次取走。
fn collect_tool_results(messages: &[Value]) -> HashMap<String, ToolOutcome> {
    let mut map: HashMap<String, ToolOutcome> = HashMap::new();
    for message in messages {
        if message.get("role").and_then(Value::as_str) != Some("toolResult") {
            continue;
        }
        let Some(call_id) = message.get("toolCallId").and_then(Value::as_str) else {
            continue;
        };
        let text = match message.get("content") {
            Some(Value::String(text)) => text.clone(),
            _ => blocks_plain_text(message.get("content")),
        };
        map.insert(
            call_id.to_string(),
            ToolOutcome {
                text,
                failed: message
                    .get("isError")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
        );
    }
    map
}

/// 一次工具调用 + 它的结果，合成**一个**折叠块。
///
/// 结构：摘要行（`→ 工具名 首行参数` + 结果行数 + 失败标记），展开后是两段
/// 「参数」与「结果」。没配上结果时明说一句，而不是默默只剩参数。
fn tool_call_block(
    block: &Value,
    results: &HashMap<String, ToolOutcome>,
    s: Strings,
) -> Option<String> {
    let (name, args) = tool_name_and_args(block);
    // 配对键：`context_view` 会把 `toolCall` 归一化成 `toolCallId`，而 v3 原文是 `id`。
    // 两个都认，否则归一化后一律配对失败（渲染成满屏「没有记录到结果」）。
    let call_id = block
        .get("toolCallId")
        .or_else(|| block.get("id"))
        .and_then(Value::as_str);
    let outcome = call_id.and_then(|id| results.get(id));
    let failed = outcome.map(|o| o.failed).unwrap_or(false);

    let hint = truncate_chars(&args_one_line(args.as_ref()), 90);
    // 徽章只放方向箭头（`→` 调用）：工具名已经紧跟在后，别重复一遍。
    let mut summary = format!(
        "<span class=\"badge\">\u{2192}</span><span class=\"tool-name\">{}</span>\
<span class=\"tool-hint\">{}</span>",
        escape_html(&name),
        escape_html(&hint)
    );
    if let Some(outcome) = outcome {
        let lines = outcome
            .text
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count();
        if lines > 0 {
            summary.push_str(&format!(
                "<span class=\"tool-count\">{}</span>",
                escape_html(&s.result_lines.replace("{n}", &lines.to_string()))
            ));
        }
    }
    if failed {
        summary.push_str(&format!(
            "<span class=\"tool-error\">{}</span>",
            escape_html(s.tool_error)
        ));
    }

    // 参数段：只有真的是对象/字符串时才占一段，空参数不渲染空标题。
    let args_html = match args.as_ref() {
        Some(value) if value.is_object() || value.is_string() => {
            let pretty = if let Some(text) = value.as_str() {
                text.to_string()
            } else {
                serde_json::to_string_pretty(value).unwrap_or_default()
            };
            if pretty.trim().is_empty() {
                String::new()
            } else {
                format!(
                    "<div class=\"tool-part\"><h4>{}</h4>{}</div>",
                    escape_html(s.arguments),
                    pre(&clamp_block(&pretty, s))
                )
            }
        }
        _ => String::new(),
    };

    // 结果段：配上就渲染输出（空输出也保留一段说明），没配上就明说。
    let result_html = match outcome {
        Some(outcome) if outcome.text.trim().is_empty() => format!(
            "<div class=\"tool-part\"><h4>{}</h4>{}</div>",
            escape_html(s.result),
            pre(s.output_empty)
        ),
        Some(outcome) => format!(
            "<div class=\"tool-part\"><h4>{}</h4>{}</div>",
            escape_html(s.result),
            pre(&clamp_block(&outcome.text, s))
        ),
        None => format!(
            "<div class=\"tool-part is-missing\"><h4>{}</h4><p>{}</p></div>",
            escape_html(s.result),
            escape_html(s.result_missing)
        ),
    };

    Some(details_block(
        if failed { "tool is-error" } else { "tool" },
        &summary,
        &format!("{args_html}{result_html}"),
        false,
    ))
}

/// 一条消息的头部（角色 + 时间）。
///
/// assistant 不写角色名：一轮里它可能出现十几次（每次调工具一条），
/// 每块都顶一行「回答」只会把主线撑成一堆重复标签。它只留时间，
/// 用户提问与自定义消息才需要角色标签来区分。
fn message_head(kind: Kind, message: &Value, s: Strings) -> String {
    let label = match kind {
        Kind::User => Some((s.user, "user")),
        Kind::Custom => Some((s.custom, "custom")),
        Kind::Assistant | Kind::Tool => None,
    };
    let clock = clock_of(message)
        .map(|time| format!("<time>{}</time>", escape_html(&time)))
        .unwrap_or_default();
    match label {
        Some((text, class)) => format!(
            "<div class=\"head {class}\"><span class=\"role\">{}</span>{clock}</div>",
            escape_html(text)
        ),
        // 只有时间时也保留这行，不然连续 assistant 块会粘成一团。
        None if !clock.is_empty() => format!("<div class=\"head quiet\">{clock}</div>"),
        None => String::new(),
    }
}

/// 会话里记录的系统提示词。
///
/// v3 文件把它存在 `message.sections` 里（`content` 是空串），键为
/// `preamble` / `tools` / `rules` / `docs` / `project_context` / `skills` / `cwd`。
/// 老的/别的写入方也可能把全文放在 `content`，两种都认。
#[derive(Default)]
struct SystemPrompt {
    /// 按展示顺序排好的 `(键, 正文)`。
    sections: Vec<(String, String)>,
    chars: usize,
    /// 这个会话里一共记了几次系统提示词（切工具 / 换模型会重发）。
    versions: usize,
}

/// 从活动分支上收集系统提示词。
///
/// **不能只取最后一条**：同一分支上往往有多条（切工具 / 改模型时重发），
/// 而后面的那些是**增量**——实测一份会话里后两条只带 `tools` + `rules`，
/// `project_context`（8 KB）与 `skills` 只在第一条里有。只读最后一条，
/// 面板上就只剩两个分节。所以按分支顺序合并，同名分节以**后者**为准。
fn system_prompt_of(entries: &[Value], leaf_id: Option<&str>) -> Option<SystemPrompt> {
    let branch = session_view::branch_entries(entries, leaf_id);
    let mut merged: Vec<(String, String)> = Vec::new();
    let mut versions = 0usize;

    for entry in &branch {
        let Some(message) = entry.get("message") else {
            continue;
        };
        if message.get("role").and_then(Value::as_str) != Some("system") {
            continue;
        }
        versions += 1;
        let Some(map) = message.get("sections").and_then(Value::as_object) else {
            continue;
        };
        for (key, value) in map {
            let Some(text) = value.as_str() else { continue };
            if text.trim().is_empty() {
                continue;
            }
            match merged.iter_mut().find(|(k, _)| k == key) {
                Some(slot) => slot.1 = text.to_string(),
                None => merged.push((key.clone(), text.to_string())),
            }
        }
    }

    // 兜回退：没有 sections（或全空）就看 content，从尾部往前找第一条非空的。
    if merged.is_empty() {
        for entry in branch.iter().rev() {
            let Some(message) = entry.get("message") else {
                continue;
            };
            if message.get("role").and_then(Value::as_str) != Some("system") {
                continue;
            }
            let text = match message.get("content") {
                Some(Value::String(text)) => text.clone(),
                Some(Value::Array(blocks)) => blocks
                    .iter()
                    .filter_map(|block| block.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => String::new(),
            };
            if text.trim().is_empty() {
                continue;
            }
            let chars = text.chars().count();
            return Some(SystemPrompt {
                sections: vec![(String::new(), text)],
                chars,
                versions,
            });
        }
        return None;
    }

    // 固定展示顺序，未知键按字母序垫后。
    const ORDER: [&str; 9] = [
        "preamble",
        "tools",
        "rules",
        "project_instructions",
        "project_context",
        "skills",
        "docs",
        "cwd",
        "env",
    ];
    merged.sort_by(|a, b| {
        let rank = |key: &str| {
            ORDER
                .iter()
                .position(|known| *known == key)
                .unwrap_or(ORDER.len())
        };
        rank(&a.0).cmp(&rank(&b.0)).then_with(|| a.0.cmp(&b.0))
    });

    let chars = merged.iter().map(|(_, text)| text.chars().count()).sum();
    Some(SystemPrompt {
        sections: merged,
        chars,
        versions,
    })
}

fn section_label(key: &str, s: Strings) -> &'static str {
    match key {
        "preamble" => s.section_preamble,
        "tools" => s.section_tools,
        "rules" => s.section_rules,
        "docs" => s.section_docs,
        "project_context" => s.section_project_context,
        "project_instructions" => s.section_project_instructions,
        "skills" => s.section_skills,
        "cwd" => s.section_cwd,
        "env" => s.section_env,
        _ => s.section_other,
    }
}

/// 系统提示词面板：每个分节一张卡片，长清单可以折起。
fn render_system_prompt(prompt: Option<&SystemPrompt>, s: Strings) -> String {
    let Some(prompt) = prompt else {
        return format!("<p class=\"empty\">{}</p>", escape_html(s.system_empty));
    };
    let mut cards = String::new();
    for (index, (key, text)) in prompt.sections.iter().enumerate() {
        let label = if key.is_empty() {
            s.system_prompt
        } else {
            section_label(key, s)
        };
        // 提示词是纯文本（带 <tools> 这类标签），不跑 markdown：它要的是
        // 「看到实际发出去的字节」，不是排版好看。
        let body = pre(text);
        // 第一个分节总展开，后面的默认收起——大会话的 project_context 能到 8 KB。
        cards.push_str(&details_block(
            "prompt-section",
            &format!(
                "<span class=\"badge\">{n}</span><span class=\"tool-name\">{label}</span>\
<span class=\"tool-hint\">{chars}</span>",
                n = index + 1,
                label = escape_html(label),
                chars = escape_html(
                    &s.system_chars
                        .replace("{count}", &text.chars().count().to_string())
                ),
            ),
            &body,
            index == 0,
        ));
    }
    let meta = if prompt.versions > 1 {
        format!(
            "{} · {}",
            s.system_chars.replace("{count}", &prompt.chars.to_string()),
            s.system_versions
                .replace("{count}", &prompt.versions.to_string())
        )
    } else {
        s.system_chars.replace("{count}", &prompt.chars.to_string())
    };
    format!("<p class=\"prompt-meta\">{}</p>{cards}", escape_html(&meta))
}

/// 一轮对话：一条用户提问 + 它之后直到下一条提问的全部内容。
struct Turn {
    /// 用户提问的锚点 id（前导段没有提问，用 `pre`）。
    id: String,
    /// 目录里显示的标题（提问首行）。
    title: String,
    /// 目录里的副标题（时间 + 轮次）。
    meta: String,
    body: String,
}

fn render_export_html(
    session: &RawSession,
    id: &str,
    lang: &str,
    overlay_chrome: bool,
    theme: Option<ExportTheme>,
) -> String {
    let s = strings_for(lang == STRINGS_ZH.lang);
    let name = session.name().unwrap_or_else(|| id.to_string());
    let cwd = crate::paths::simplified(&session.header.cwd);
    let view = session_view::context_view(&session.entries, None);

    // 按「用户提问」切轮次。工具输出占真实会话的 76%，跟正文平铺在一起
    // 主线就被淹了；这一层的全部作用就是把主线（提问 + 回答）抬出来。
    //
    // 编号规则：第一条提问之前的零散内容归一个前导段（`turn-0`），
    // 之后的每个提问各占一轮（`turn-1`、`turn-2`…）。编号必须按实际轮次
    // 分配，不能拿 `turns.len()+1` 算——那样前导段会把编号顶错。
    // 工具结果先一次性配对好：v3 里调用与结果是两条独立消息，只靠
    // `toolCall.id` ↔ `toolResult.toolCallId` 关联，不先配对就只能靠相邻摆放暗示关系。
    let tool_results = collect_tool_results(&view.messages);

    let mut turns: Vec<Turn> = Vec::new();
    let mut preamble_done = false;
    let mut round = 0usize;
    for message in &view.messages {
        let kind = kind_of(message);
        let is_prompt = kind == Kind::User;
        if is_prompt {
            round += 1;
        }
        let need_new_turn = is_prompt || turns.is_empty();
        if need_new_turn {
            let raw_title = blocks_plain_text(message.get("content"))
                .lines()
                .map(str::trim)
                .find(|line| !line.is_empty())
                .unwrap_or("")
                .to_string();
            // 纯图片的提问没有正文，标题给出可辨认的占位。
            let title = if raw_title.is_empty() {
                if is_prompt {
                    s.image.to_string()
                } else {
                    s.preamble.to_string()
                }
            } else {
                truncate_chars(&raw_title, 80)
            };
            let anchor = if is_prompt {
                format!("turn-{round}")
            } else {
                preamble_done = true;
                "turn-0".to_string()
            };
            let clock = clock_of(message).unwrap_or_default();
            turns.push(Turn {
                id: anchor,
                title,
                meta: if is_prompt {
                    format!("{} · {}", clock, s.round.replace("{n}", &round.to_string()))
                } else {
                    format!("{} · {}", clock, s.preamble)
                },
                body: String::new(),
            });
        } else if preamble_done && turns.len() == 1 {
            // 静默：前导段之后、第一条提问之前的内容仍然属于 `turn-0`。
        }

        // toolResult 的内容已经并进对应 toolCall 的折叠块，这里整条跳过：
        // 既不再单占一块，也不会顶一行「Tool 时间」把主线撑开。
        if kind == Kind::Tool {
            continue;
        }

        let (main, aside) = render_message(message, &tool_results, s);
        let turn = turns.last_mut().expect("至少一轮");
        if !main.is_empty() || !aside.is_empty() {
            turn.body.push_str(&format!(
                "<article class=\"msg {class}\">{head}<div class=\"main\">{main}</div>\
<div class=\"aside\">{aside}</div></article>",
                class = match kind {
                    Kind::User => "user",
                    Kind::Assistant => "assistant",
                    Kind::Custom => "custom",
                    Kind::Tool => "toolout",
                },
                head = message_head(kind, message, s),
            ));
        }
    }
    // 前导段如果只有工具调用这类“没有正文”的东西，就没必要单占一个目录项。
    // （真正的空轮次不会出现：每个 turn 都是被某条消息开出来的。）
    turns.retain(|turn| !turn.body.is_empty());

    // 左侧大纲（常驻，不再需要点击展开）。系统提示词也作为一个导航项。
    let system_prompt = system_prompt_of(&session.entries, None);
    let mut outline = String::new();
    if system_prompt.is_some() {
        outline.push_str(&format!(
            "<a class=\"toc-item toc-system\" href=\"#system-prompt\">\
<span class=\"toc-title\">{}</span><span class=\"toc-meta\">{}</span></a>",
            escape_html(s.system_prompt),
            escape_html(
                &s.system_chars.replace(
                    "{count}",
                    &system_prompt
                        .as_ref()
                        .map(|p| p.chars)
                        .unwrap_or(0)
                        .to_string()
                )
            ),
        ));
    }
    let mut body = String::new();
    if system_prompt.is_some() {
        body.push_str(&format!(
            "<section class=\"system-block\" id=\"system-prompt\">\
<h2 class=\"turn-head\">{}</h2>{}</section>",
            escape_html(s.system_prompt),
            render_system_prompt(system_prompt.as_ref(), s),
        ));
    }
    for turn in &turns {
        outline.push_str(&format!(
            "<a class=\"toc-item\" href=\"#{id}\"><span class=\"toc-title\">{title}</span>\
<span class=\"toc-meta\">{meta}</span></a>",
            id = escape_html(&turn.id),
            title = escape_html(&turn.title),
            meta = escape_html(&turn.meta),
        ));
        body.push_str(&format!(
            "<section class=\"turn\" id=\"{id}\"><h2 class=\"turn-head\">{title}</h2>\
{content}</section>",
            id = escape_html(&turn.id),
            title = escape_html(&turn.title),
            content = turn.body,
        ));
    }
    if turns.is_empty() {
        body.push_str(&format!(
            "<p class=\"empty\">{}</p>",
            escape_html(s.no_messages)
        ));
    }

    // `<html>` 上的类名：`overlay-chrome`（桌面壳红绿灯让位）与
    // `theme-light` / `theme-dark`（应用里显式选的主题，优先级高于系统偏好）。
    // 两两可以共存，所以拼成一个 class 字符串，不能各自写一个 `class=`。
    let mut root_classes: Vec<&str> = Vec::new();
    if overlay_chrome {
        root_classes.push("overlay-chrome");
    }
    if let Some(theme) = theme {
        root_classes.push(theme.root_class());
    }
    let root_class = if root_classes.is_empty() {
        String::new()
    } else {
        format!(" class=\"{}\"", root_classes.join(" "))
    };
    // 显式主题时告诉浏览器原生部件（滚动条、输入框）该用哪种配色；
    // 跟随系统时留空，让浏览器自己判断。
    let color_scheme = theme
        .map(|theme| {
            format!(
                "<meta name=\"color-scheme\" content=\"{}\">",
                theme.color_scheme()
            )
        })
        .unwrap_or_default();

    format!(
        "<!doctype html>\n<html lang=\"{lang}\"{root_class}><head><meta charset=\"utf-8\">\
<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\">\
{color_scheme}<title>{title}</title>\
<meta name=\"generator\" content=\"toiler\">\
<style>{style}</style></head><body>\
<div class=\"shell\">\
<aside class=\"outline\"><div class=\"outline-inner\">\
<p class=\"outline-head\">{label}<span class=\"outline-count\">{n}</span></p>\
<nav class=\"toc-list\">{items}</nav></div></aside>\
<div class=\"column\">\
<header><h1>{title}</h1>\
<p class=\"meta\">{cwd} · {count} {messages} · {turns}</p></header>\
<main>{body}</main></div></div></body></html>\n",
        lang = escape_html(s.lang),
        root_class = root_class,
        color_scheme = color_scheme,
        title = escape_html(&name),
        cwd = escape_html(&cwd),
        count = view.messages.len(),
        messages = escape_html(s.messages),
        turns = escape_html(&s.round.replace("{n}", &turns.len().to_string())),
        label = escape_html(s.outline),
        n = turns.len(),
        items = outline,
        body = body,
        style = export_style(),
    )
}

// ---------------------------------------------------------------------------
// auto-name
// ---------------------------------------------------------------------------

/// 标题生成用的提示词（逐字取自 pi 的 `79495.i`）。
const TITLE_PROMPT: &str =
    "Create a concise title for this session based on the conversation above.

Requirements:
- Match the primary language used by the user.
- Describe the user's concrete goal or the outcome, not the act of chatting.
- Use 4-12 words for space-separated languages, or 8-24 characters for CJK text when practical.
- Do not call any tools.
- Return only the title as plain text, with no quotes, label, markdown, or explanation.";

/// `POST /api/sessions/{id}/auto-name`
pub async fn auto_name(
    State(state): State<AppState>,
    _trusted: Trusted,
    Path(id): Path<String>,
) -> Response {
    let Some(path) = state.sessions().path_of(&id) else {
        return error(StatusCode::NOT_FOUND, "Session not found");
    };
    let Ok(session) = read_session(&path) else {
        return error(StatusCode::INTERNAL_SERVER_ERROR, "读会话失败");
    };

    let view = session_view::context_view(&session.entries, None);
    let transcript = render_transcript(&view.messages);
    if transcript.trim().is_empty() {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "The session has no user messages to name",
        );
    }

    // 模型：会话里最后一个 `model_change`，否则目录默认
    let (provider, model_id) = session_model(&state, &view.model);
    let Some((runtime, model)) = lookup_model(&state, &provider, &model_id) else {
        return error(
            StatusCode::INTERNAL_SERVER_ERROR,
            "No available model configured for title generation",
        );
    };
    let api_key = resolve_key(&state, &provider);
    let prompt = format!("{transcript}\n\n{TITLE_PROMPT}");

    let result = crate::oneshot::complete(
        runtime,
        &model,
        api_key,
        prompt,
        256,
        std::time::Duration::from_secs(90),
    )
    .await;
    let reply = match result {
        Ok(reply) => reply,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    let title = match clean_title(&reply.text) {
        Ok(title) => title,
        Err(message) => return error(StatusCode::INTERNAL_SERVER_ERROR, &message),
    };

    // 写回会话名（pi 的 `setSessionName` → 追加 `session_info` 条目）
    let storage = match toiler_session::V3SessionStorage::load(&path) {
        Ok(storage) => storage,
        Err(e) => return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
    };
    {
        use rpi_harness::session::types::SessionStorage;
        if let Err(e) = storage.set_name(Some(&title)).await {
            return error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string());
        }
    }
    state.sessions().invalidate();

    let usage = reply.usage.map(|usage| {
        json!({
            "input": usage.input,
            "output": usage.output,
            "cacheRead": usage.cache_read,
            "cacheWrite": usage.cache_write,
            "total": usage.total_tokens,
        })
    });
    Json(json!({ "title": title, "usage": usage })).into_response()
}

fn session_model(state: &AppState, model: &Value) -> (String, String) {
    if let (Some(provider), Some(model_id)) = (
        model.get("provider").and_then(Value::as_str),
        model.get("modelId").and_then(Value::as_str),
    ) {
        if !provider.is_empty() && !model_id.is_empty() {
            return (provider.to_string(), model_id.to_string());
        }
    }
    state
        .default_model_ref()
        .unwrap_or_else(|| (String::new(), String::new()))
}

fn lookup_model(
    state: &AppState,
    provider: &str,
    model_id: &str,
) -> Option<(std::sync::Arc<dyn rpi_ai::Provider>, rpi_ai::Model)> {
    let runtime = state
        .catalog()
        .providers
        .iter()
        .find(|p| p.id() == provider)
        .cloned()?;
    let model = state.catalog().model_for(provider, model_id)?.clone();
    Some((runtime, model))
}

fn resolve_key(state: &AppState, provider: &str) -> Option<String> {
    if let Some(key) = state.auth().api_key(provider) {
        return Some(key);
    }
    let root = state.models_file().read();
    let config = crate::models_json::provider_config(&root, provider)?;
    match crate::auth::resolve_key_config(config.get("apiKey")?) {
        crate::auth::ResolvedKey::Literal(key) => Some(key),
        _ => None,
    }
}

/// 把 context 消息压成纯文本对话（只要 user / assistant 的文字）。
fn render_transcript(messages: &[Value]) -> String {
    let mut out = String::new();
    for message in messages {
        let role = message.get("role").and_then(Value::as_str).unwrap_or("");
        let (label, text) = match role {
            "user" => ("User", text_of(message.get("content"))),
            "assistant" => ("Assistant", text_of(message.get("content"))),
            "custom" => ("Note", text_of(message.get("content"))),
            _ => continue,
        };
        if text.trim().is_empty() {
            continue;
        }
        out.push_str(&format!("{label}: {text}\n\n"));
    }
    // 只保留最后一段，避免超长会话把上下文吃满
    const MAX_CHARS: usize = 12_000;
    if out.chars().count() > MAX_CHARS {
        let tail: String = out
            .chars()
            .rev()
            .take(MAX_CHARS)
            .collect::<String>()
            .chars()
            .rev()
            .collect();
        return tail;
    }
    out
}

fn text_of(content: Option<&Value>) -> String {
    let Some(content) = content else {
        return String::new();
    };
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(blocks) = content.as_array() else {
        return String::new();
    };
    blocks
        .iter()
        .filter_map(|block| {
            if block.get("type").and_then(Value::as_str) == Some("text") {
                block.get("text").and_then(Value::as_str)
            } else {
                None
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 清洗模型返回的标题（pi 的 `79495` 里那段 `function(a)`）。
pub fn clean_title(raw: &str) -> Result<String, String> {
    let mut text = raw.trim().to_string();
    if let Some(stripped) = strip_code_fence(&text) {
        text = stripped;
    }
    if text.starts_with('{') {
        if let Ok(value) = serde_json::from_str::<Value>(&text) {
            if let Some(title) = value.get("title").and_then(Value::as_str) {
                text = title.trim().to_string();
            }
        }
    }
    // 只要第一行
    text = text.lines().next().unwrap_or("").to_string();
    // 去掉 `Title:` / `标题：` 前缀
    let lowered = text.to_lowercase();
    for prefix in ["session title", "title", "标题"] {
        if lowered.starts_with(prefix) {
            let rest = &text[prefix.len()..];
            let trimmed = rest.trim_start();
            if let Some(stripped) = trimmed.strip_prefix([':', '：', '-']) {
                text = stripped.trim().to_string();
            }
        }
    }
    // 去掉成对引号
    for (open, close) in [
        ('"', '"'),
        ('\'', '\''),
        ('`', '`'),
        ('“', '”'),
        ('「', '」'),
        ('『', '』'),
    ] {
        let chars: Vec<char> = text.chars().collect();
        if chars.len() > 2 && chars[0] == open && chars[chars.len() - 1] == close {
            text = chars[1..chars.len() - 1]
                .iter()
                .collect::<String>()
                .trim()
                .to_string();
        }
    }
    text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text = text
        .trim_end_matches(['。', '.', '!', '！'])
        .trim()
        .to_string();
    if !text.chars().any(|c| c.is_alphanumeric()) {
        return Err("The model did not return a usable session title".to_string());
    }
    let chars: Vec<char> = text.chars().collect();
    if chars.len() > 80 {
        return Ok(chars[..80].iter().collect::<String>().trim().to_string());
    }
    Ok(text)
}

fn strip_code_fence(text: &str) -> Option<String> {
    let trimmed = text.trim();
    let after_open = trimmed
        .strip_prefix("```json")
        .or_else(|| trimmed.strip_prefix("```text"))
        .or_else(|| trimmed.strip_prefix("```"))?;
    let body = after_open.strip_suffix("```")?;
    Some(body.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleans_generated_titles() {
        assert_eq!(clean_title("修复登录 bug").unwrap(), "修复登录 bug");
        assert_eq!(clean_title("\"Fix login bug\"").unwrap(), "Fix login bug");
        assert_eq!(clean_title("标题：缓存方案设计。").unwrap(), "缓存方案设计");
        assert_eq!(
            clean_title("```json\n{\"title\":\"Refactor auth\"}\n```").unwrap(),
            "Refactor auth"
        );
        assert_eq!(
            clean_title("Title: Add SSE route").unwrap(),
            "Add SSE route"
        );
        assert!(clean_title("   ").is_err());
        assert!(clean_title("!!!").is_err());
    }

    #[test]
    fn truncates_long_titles() {
        let long = "a".repeat(120);
        assert_eq!(clean_title(&long).unwrap().chars().count(), 80);
    }

    #[test]
    fn sanitizes_download_names() {
        assert_eq!(
            sanitize_ascii_filename("pi-session-x.html"),
            "pi-session-x.html"
        );
        assert_eq!(sanitize_ascii_filename("名字.html"), "__.html");
        assert_eq!(percent_encode("a b.html"), "a%20b.html");
    }

    #[test]
    fn renders_transcript_text_only() {
        let messages = vec![
            json!({"role": "user", "content": [{"type": "text", "text": "hi"}]}),
            json!({"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hmm"},
                {"type": "text", "text": "hello"}
            ]}),
            json!({"role": "toolResult", "content": "ignored"}),
        ];
        let text = render_transcript(&messages);
        assert!(text.contains("User: hi"));
        assert!(text.contains("Assistant: hello"));
        assert!(!text.contains("hmm"));
        assert!(!text.contains("ignored"));
    }
}
