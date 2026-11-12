//! 阶段 5 回归：把 pi 前端测试断言的接口行为固化成 Rust 契约。
//!
//! 这里**不复刻** pi 的测试文件（那些断言针对 TS 源码，这里没有意义），而是把
//! 「前端真的依赖的行为」逐条落成对 Router 的断言。每条用例的注释都写着它对应
//! 前端的哪一处用法，出问题时能直接反查。
//!
//! 覆盖范围（按前端调用频率排序）：
//! 1. `/api/files` 的六种 `type` —— 文件面板、查看器、Markdown 内嵌图全靠它
//! 2. `/api/files` 写侧 —— 编辑器保存、重命名、拖拽上传
//! 3. `/api/mobile/access` + `/api/mobile/pair` —— 设置页的「手机访问」
//! 4. `/api/sessions*` 的字段与分页
//! 5. `/api/models` / `/api/projects` / `/api/cwd/validate`

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use tower::ServiceExt;

use toiler_server::{catalog::ModelCatalog, routes, state::AppState};

// ============================================================================
// 脚手架
// ============================================================================

struct Reply {
    status: StatusCode,
    headers: HeaderMap,
    body: Vec<u8>,
}

impl Reply {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).to_string()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| {
            panic!("响应不是 JSON（{error}）：{}", self.text());
        })
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).and_then(|value| value.to_str().ok())
    }
}

/// 改进程级环境变量的测试要串行跑。
///
/// `cargo test` 默认多线程，而 `TOILER_AGENT_DIR` / `TOILER_NPM` 是全局的：
/// 一个测试把它们指向临时目录的窗口期内，邻接测试会跟着读到那个值（曾经让
/// `mobile_access_and_pair_share_one_contract` 偶发失败）。需要隔离目录的测试持有
/// 这把锁，改完立刻恢复。
static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn fixture_root(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "toiler-regress-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn app(sessions_root: &Path, cwd: &Path) -> axum::Router {
    app_with_agent_dir(sessions_root, cwd, None)
}

/// 建一个 Router。
///
/// `agent_dir` 给定时，项目库（`work.sqlite`）、`settings.json`、`trust.json` 等
/// **全部指到那个隔离目录**。这一步不能省：默认的 `ProjectStore::default()` 指向
/// 用户真实的 `~/.agents/toiler/work.sqlite`，`POST /api/cwd/validate` 会往里
/// 登记项目 —— 阶段 5 第一次跑契约测试时就污染过用户 18 条项目记录（已清理）。
fn app_with_agent_dir(sessions_root: &Path, cwd: &Path, agent_dir: Option<&Path>) -> axum::Router {
    let catalog = ModelCatalog::from_config(json!({ "providers": {} }), None).unwrap();
    let mut state =
        AppState::with_cwd(sessions_root, catalog, cwd).with_binding("127.0.0.1", 30141);
    if let Some(dir) = agent_dir {
        state = state.with_stores(
            toiler_server::auth::AuthStore::new(dir.join("auth.json")),
            toiler_server::models_json::ModelsFile::at(dir.join("models.json")),
            toiler_server::settings::SettingsFile::at(dir.join("settings.json")),
            toiler_server::projects::ProjectStore::at(dir.join("work.sqlite")),
            toiler_server::trust::TrustStore::at(dir.join("trust.json")),
        );
    }
    routes::router(state)
}

async fn call(
    router: axum::Router,
    method: &str,
    uri: &str,
    body: Option<&str>,
    headers: &[(&str, &str)],
) -> Reply {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header("host", "127.0.0.1:30141");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let request = match body {
        Some(text) => builder
            .header("content-type", "application/json")
            .body(Body::from(text.to_string())),
        None => builder.body(Body::empty()),
    }
    .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

async fn get(router: axum::Router, uri: &str) -> Reply {
    call(router, "GET", uri, None, &[]).await
}

/// 把绝对路径编码成前端 `encodeFilePathForApi` 的形状（去前导斜杠，逐段编码）。
fn encode_path(path: &Path) -> String {
    let raw = path.to_string_lossy().replace('\\', "/");
    raw.split('/')
        .filter(|segment| !segment.is_empty())
        .map(url_encode)
        .collect::<Vec<_>>()
        .join("/")
}

fn url_encode(segment: &str) -> String {
    let mut out = String::new();
    for byte in segment.as_bytes() {
        let ch = *byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '~') {
            out.push(ch);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}

// ============================================================================
// 1) /api/files —— 六种 type
// ============================================================================

#[tokio::test]
async fn files_list_hides_ignored_names_and_marks_git_roots() {
    // 前端 FileExplorer 依赖：`entries[].isDir`、`gitRepository`、排序（目录优先）。
    let root = fixture_root("list");
    std::fs::create_dir_all(root.join("src")).unwrap();
    std::fs::create_dir_all(root.join("node_modules/pkg")).unwrap();
    std::fs::create_dir_all(root.join("app/.git")).unwrap();
    std::fs::write(root.join("README.md"), "# hi").unwrap();
    std::fs::write(root.join("main.pyc"), "x").unwrap();
    std::fs::write(root.join(".hidden"), "x").unwrap();

    let url = format!("/api/files/{}/", encode_path(&root));
    let reply = get(app(&root, &root), &url).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());

    let body = reply.json();
    let entries = body["entries"].as_array().unwrap();
    let names: Vec<&str> = entries.iter().filter_map(|e| e["name"].as_str()).collect();

    assert!(!names.contains(&"node_modules"), "忽略名单里的目录不出现");
    assert!(!names.contains(&"main.pyc"), "忽略后缀不出现");
    assert!(
        names.contains(&".hidden"),
        "点开头默认可见（hideDotDirs=false）"
    );
    assert!(names.contains(&"README.md"));

    // 目录优先，且 app 带 gitRepository
    assert_eq!(names.first(), Some(&"app"), "目录排在文件前");
    let app_entry = entries.iter().find(|e| e["name"] == "app").unwrap();
    assert_eq!(app_entry["isDir"], true);
    assert_eq!(app_entry["gitRepository"], true);
    let readme = entries.iter().find(|e| e["name"] == "README.md").unwrap();
    assert_eq!(readme["isDir"], false);
    assert!(readme.get("gitRepository").is_none());

    // list 不 stat 文件：size/modified 恒为 0 / ""
    assert_eq!(readme["size"], 0);
    assert_eq!(readme["modified"], "");

    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_list_hides_dot_entries_when_asked() {
    // 前端只在项目根传 `hideDotDirs=true`。
    let root = fixture_root("dots");
    std::fs::write(root.join(".env"), "A=1").unwrap();
    std::fs::write(root.join("a.txt"), "x").unwrap();

    let url = format!(
        "/api/files/{}?type=list&hideDotDirs=true",
        encode_path(&root)
    );
    let body = get(app(&root, &root), &url).await.json();
    let names: Vec<&str> = body["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert_eq!(names, vec!["a.txt"]);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_read_returns_text_json_with_language() {
    // 前端 FileViewer 用 `{content, language, size}` 选语法高亮与 Markdown 渲染。
    let root = fixture_root("readtext");
    std::fs::write(root.join("x.md"), "# 标题\n").unwrap();

    let url = format!("/api/files/{}?type=read", encode_path(&root.join("x.md")));
    let reply = get(app(&root, &root), &url).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.header("content-type"), Some("application/json"));

    let body = reply.json();
    assert_eq!(body["content"], "# 标题\n");
    assert_eq!(body["language"], "markdown");
    assert_eq!(body["size"], 9); // "# 标题\n" = 2 + 3 + 1 + ... 按字节算
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_read_streams_images_as_bytes() {
    // Markdown 内嵌图直接 `<img src="/api/files/…?type=read">`，必须回原始字节。
    let root = fixture_root("readimg");
    // 最小合法 PNG（1×1 透明）
    let png: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48, 0x44,
        0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00, 0x00, 0x1f,
        0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78, 0x9c, 0x63, 0x00,
        0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00, 0x00, 0x00, 0x00, 0x49,
        0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];
    std::fs::write(root.join("p.png"), &png).unwrap();

    let url = format!("/api/files/{}?type=read", encode_path(&root.join("p.png")));
    let reply = get(app(&root, &root), &url).await;
    assert_eq!(reply.status, StatusCode::OK);
    assert_eq!(reply.header("content-type"), Some("image/png"));
    assert_eq!(reply.body, png, "必须逐字节返回，不能被转成 base64 JSON");
    assert_eq!(reply.header("accept-ranges"), Some("bytes"));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_read_supports_range_requests() {
    // 音视频拖动进度条、以及浏览器对 PDF 的分段读取。
    let root = fixture_root("range");
    // 用 `.m4a`（pi 的 AUDIO_EXT_TO_MIME 里有它）：`.mp4` 不在表里会被当文本读。
    std::fs::write(root.join("a.m4a"), vec![7u8; 1000]).unwrap();
    let url = format!("/api/files/{}?type=read", encode_path(&root.join("a.m4a")));

    let reply = call(
        app(&root, &root),
        "GET",
        &url,
        None,
        &[("range", "bytes=100-199")],
    )
    .await;
    assert_eq!(reply.status, StatusCode::PARTIAL_CONTENT);
    assert_eq!(reply.header("content-range"), Some("bytes 100-199/1000"));
    assert_eq!(reply.header("content-length"), Some("100"));
    assert_eq!(reply.body.len(), 100);

    // 越界 → 416 + `bytes */size`
    let bad = call(
        app(&root, &root),
        "GET",
        &url,
        None,
        &[("range", "bytes=5000-")],
    )
    .await;
    assert_eq!(bad.status, StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(bad.header("content-range"), Some("bytes */1000"));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_read_rejects_oversized_text_previews() {
    // pi 的上限是 256KB，超过就 413（前端会显示「文件过大」）。
    let root = fixture_root("bigtext");
    std::fs::write(root.join("big.txt"), vec![b'a'; 300 * 1024]).unwrap();
    let url = format!(
        "/api/files/{}?type=read",
        encode_path(&root.join("big.txt"))
    );
    let reply = get(app(&root, &root), &url).await;
    assert_eq!(reply.status, StatusCode::PAYLOAD_TOO_LARGE);
    assert!(reply.text().contains("256KB"), "{}", reply.text());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_meta_reports_the_fields_the_watcher_polls() {
    // FileViewer 的 watch 回调只读 `{size}`；DOCX 预览额外要 `previewKind`。
    let root = fixture_root("meta");
    std::fs::write(root.join("a.docx"), b"not really a docx").unwrap();
    let url = format!("/api/files/{}?type=meta", encode_path(&root.join("a.docx")));
    let body = get(app(&root, &root), &url).await.json();

    assert_eq!(body["size"], 17, "'not really a docx' 是 17 字节");
    assert_eq!(body["language"], "word");
    assert_eq!(body["previewKind"], "docx");
    assert_eq!(
        body["mime"],
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_download_is_an_attachment_with_a_utf8_name() {
    let root = fixture_root("download");
    std::fs::write(root.join("计划.md"), "# x").unwrap();
    let url = format!(
        "/api/files/{}?type=download",
        encode_path(&root.join("计划.md"))
    );
    let reply = get(app(&root, &root), &url).await;
    assert_eq!(reply.status, StatusCode::OK);
    let disposition = reply.header("content-disposition").unwrap().to_string();
    assert!(disposition.starts_with("attachment;"), "{disposition}");
    assert!(
        disposition.contains("filename*=UTF-8''%E8%AE%A1%E5%88%92.md"),
        "{disposition}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_preview_renders_docx_to_html() {
    // 前端把 `?type=preview` 塞进 iframe；非 docx 必须 400（它自己按扩展名分流）。
    let root = fixture_root("preview");
    write_minimal_docx(&root.join("doc.docx"));
    let url = format!(
        "/api/files/{}?type=preview",
        encode_path(&root.join("doc.docx"))
    );
    let reply = get(app(&root, &root), &url).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(
        reply.header("content-type"),
        Some("text/html; charset=utf-8")
    );
    let html = reply.text();
    assert!(html.contains("<h1>标题一</h1>"), "{html}");
    assert!(html.contains("<strong>加粗</strong>"), "{html}");
    assert!(html.contains("<td>值</td>"), "{html}");
    // 与 pi 同一套 CSP：预览页不许跑脚本
    let csp = reply.header("content-security-policy").unwrap();
    assert!(csp.contains("default-src 'none'"), "{csp}");

    // 非 docx → 400
    std::fs::write(root.join("a.txt"), "x").unwrap();
    let other = format!(
        "/api/files/{}?type=preview",
        encode_path(&root.join("a.txt"))
    );
    assert_eq!(
        get(app(&root, &root), &other).await.status,
        StatusCode::BAD_REQUEST
    );
    std::fs::remove_dir_all(&root).ok();
}

/// 手搓一个最小 DOCX（标题 + 粗体 + 表格），避免测试依赖外部样本文件。
fn write_minimal_docx(path: &Path) {
    let document = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>
<w:p><w:pPr><w:pStyle w:val="Heading1"/></w:pPr><w:r><w:t>标题一</w:t></w:r></w:p>
<w:p><w:r><w:rPr><w:b/></w:rPr><w:t>加粗</w:t></w:r></w:p>
<w:tbl><w:tr><w:tc><w:p><w:r><w:t>值</w:t></w:r></w:p></w:tc></w:tr></w:tbl>
</w:body></w:document>"#;

    let mut buffer = Vec::new();
    {
        let mut zip = SimpleZip::new(&mut buffer);
        zip.add("word/document.xml", document.as_bytes());
    }
    std::fs::write(path, buffer).unwrap();
}

/// 极简 zip 写入器（stored，不压缩）：测试只需要「能被解析器读回来」。
struct SimpleZip<'a> {
    out: &'a mut Vec<u8>,
    entries: Vec<(String, u32, u32)>,
}

impl<'a> SimpleZip<'a> {
    fn new(out: &'a mut Vec<u8>) -> Self {
        Self {
            out,
            entries: Vec::new(),
        }
    }

    fn add(&mut self, name: &str, data: &[u8]) {
        let offset = self.out.len() as u32;
        // local file header（method 0 = stored，size 与 crc 留 0：解析器不用它们）
        self.out.extend_from_slice(&0x0403_4b50u32.to_le_bytes());
        self.out.extend_from_slice(&20u16.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // method = stored
        self.out.extend_from_slice(&0u16.to_le_bytes()); // mtime
        self.out.extend_from_slice(&0u16.to_le_bytes()); // mdate
        self.out.extend_from_slice(&0u32.to_le_bytes()); // crc
        self.out
            .extend_from_slice(&(data.len() as u32).to_le_bytes());
        self.out
            .extend_from_slice(&(data.len() as u32).to_le_bytes());
        self.out
            .extend_from_slice(&(name.len() as u16).to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes());
        self.out.extend_from_slice(name.as_bytes());
        self.out.extend_from_slice(data);
        self.entries
            .push((name.to_string(), data.len() as u32, offset));
    }
}

impl Drop for SimpleZip<'_> {
    fn drop(&mut self) {
        let central_start = self.out.len() as u32;
        for (name, size, offset) in &self.entries {
            self.out.extend_from_slice(&0x0201_4b50u32.to_le_bytes());
            self.out.extend_from_slice(&20u16.to_le_bytes());
            self.out.extend_from_slice(&20u16.to_le_bytes());
            self.out.extend_from_slice(&0u16.to_le_bytes());
            self.out.extend_from_slice(&0u16.to_le_bytes()); // method = stored
            self.out.extend_from_slice(&0u16.to_le_bytes());
            self.out.extend_from_slice(&0u16.to_le_bytes());
            self.out.extend_from_slice(&0u32.to_le_bytes()); // crc
            self.out.extend_from_slice(&size.to_le_bytes());
            self.out.extend_from_slice(&size.to_le_bytes());
            self.out
                .extend_from_slice(&(name.len() as u16).to_le_bytes());
            self.out.extend_from_slice(&0u16.to_le_bytes()); // extra
            self.out.extend_from_slice(&0u16.to_le_bytes()); // comment
            self.out.extend_from_slice(&0u16.to_le_bytes()); // disk
            self.out.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
            self.out.extend_from_slice(&0u32.to_le_bytes()); // external attrs
            self.out.extend_from_slice(&offset.to_le_bytes());
            self.out.extend_from_slice(name.as_bytes());
        }
        let central_size = self.out.len() as u32 - central_start;
        self.out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // this disk
        self.out.extend_from_slice(&0u16.to_le_bytes()); // central disk
        self.out
            .extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        self.out
            .extend_from_slice(&(self.entries.len() as u16).to_le_bytes());
        self.out.extend_from_slice(&central_size.to_le_bytes());
        self.out.extend_from_slice(&central_start.to_le_bytes());
        self.out.extend_from_slice(&0u16.to_le_bytes()); // comment len
    }
}

#[tokio::test]
async fn files_watch_emits_named_sse_events() {
    // 前端 `es.addEventListener("connected" / "change")` —— 事件名错了就永远收不到。
    let root = fixture_root("watch");
    std::fs::write(root.join("a.txt"), "v1").unwrap();
    let url = format!("/api/files/{}?type=watch", encode_path(&root.join("a.txt")));

    let request = Request::builder()
        .uri(&url)
        .header("host", "127.0.0.1:30141")
        .body(Body::empty())
        .unwrap();
    let response = app(&root, &root).oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok()),
        Some("text/event-stream")
    );
    assert_eq!(
        response
            .headers()
            .get("cache-control")
            .and_then(|v| v.to_str().ok()),
        Some("no-cache, no-transform")
    );

    // 读第一帧：必须是 `connected`（前端收到它才去快照内容）
    use futures::StreamExt;
    let mut stream = response.into_body().into_data_stream();
    let first = tokio::time::timeout(std::time::Duration::from_secs(2), stream.next())
        .await
        .expect("应立刻收到 connected 帧")
        .expect("流不应空")
        .unwrap();
    let text = String::from_utf8_lossy(&first).to_string();
    assert!(text.contains("event: connected"), "{text:?}");
    assert!(text.contains("\"filePath\""), "{text:?}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_get_refuses_paths_outside_allowed_roots() {
    let root = fixture_root("deny");
    let reply = get(app(&root, &root), "/api/files/etc/hosts?type=read").await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN);
    assert_eq!(reply.json()["error"], "Access denied");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_get_rejects_unknown_types_and_dotdot_escapes() {
    let root = fixture_root("badtype");
    std::fs::write(root.join("a.txt"), "x").unwrap();

    let bad = format!("/api/files/{}?type=diff", encode_path(&root.join("a.txt")));
    assert_eq!(
        get(app(&root, &root), &bad).await.status,
        StatusCode::BAD_REQUEST
    );

    let escape = format!("/api/files/{}/../etc/hosts", encode_path(&root));
    assert_eq!(
        get(app(&root, &root), &escape).await.status,
        StatusCode::BAD_REQUEST
    );
    std::fs::remove_dir_all(&root).ok();
}

// ============================================================================
// 2) /api/files 写侧
// ============================================================================

#[tokio::test]
async fn files_put_saves_content_and_reports_size() {
    // FileViewer 的保存：`PUT ?type=read` + `{content, allowEmpty}` → `{size}`。
    let root = fixture_root("write");
    std::fs::write(root.join("a.txt"), "old").unwrap();
    let url = format!("/api/files/{}?type=read", encode_path(&root.join("a.txt")));

    let reply = call(
        app(&root, &root),
        "PUT",
        &url,
        Some(r#"{"content":"new content"}"#),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert_eq!(reply.json()["size"], 11);
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "new content"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_put_blocks_accidental_empty_overwrites() {
    let root = fixture_root("writeempty");
    std::fs::write(root.join("a.txt"), "old").unwrap();
    let url = format!("/api/files/{}?type=read", encode_path(&root.join("a.txt")));

    let blocked = call(
        app(&root, &root),
        "PUT",
        &url,
        Some(r#"{"content":""}"#),
        &[],
    )
    .await;
    assert_eq!(blocked.status, StatusCode::CONFLICT);
    let body = blocked.json();
    assert_eq!(body["code"], "empty_overwrite_blocked");
    assert_eq!(
        std::fs::read_to_string(root.join("a.txt")).unwrap(),
        "old",
        "被拦住时不能动文件"
    );

    // 显式允许后放行
    let allowed = call(
        app(&root, &root),
        "PUT",
        &url,
        Some(r#"{"content":"","allowEmpty":true}"#),
        &[],
    )
    .await;
    assert_eq!(allowed.status, StatusCode::OK);
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_put_rejects_oversized_and_non_string_content() {
    let root = fixture_root("writebig");
    std::fs::write(root.join("a.txt"), "old").unwrap();
    let url = format!("/api/files/{}?type=read", encode_path(&root.join("a.txt")));

    let no_content = call(
        app(&root, &root),
        "PUT",
        &url,
        Some(r#"{"content":1}"#),
        &[],
    )
    .await;
    assert_eq!(no_content.status, StatusCode::BAD_REQUEST);

    let huge = format!(r#"{{"content":"{}"}}"#, "a".repeat(300 * 1024));
    let too_big = call(app(&root, &root), "PUT", &url, Some(&huge), &[]).await;
    assert_eq!(too_big.status, StatusCode::PAYLOAD_TOO_LARGE);
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_patch_renames_and_refuses_occupied_destinations() {
    // FileExplorer 的重命名：`PATCH {name}` → `{path, name}`；撞名 409 already_exists。
    let root = fixture_root("rename");
    std::fs::write(root.join("a.txt"), "x").unwrap();
    std::fs::write(root.join("b.txt"), "y").unwrap();
    let url = format!("/api/files/{}", encode_path(&root.join("a.txt")));

    let ok = call(
        app(&root, &root),
        "PATCH",
        &url,
        Some(r#"{"name":"c.txt"}"#),
        &[],
    )
    .await;
    assert_eq!(ok.status, StatusCode::OK, "{}", ok.text());
    let body = ok.json();
    assert_eq!(body["name"], "c.txt");
    assert!(body["path"].as_str().unwrap().ends_with("c.txt"));
    assert!(root.join("c.txt").exists() && !root.join("a.txt").exists());

    let conflict = call(
        app(&root, &root),
        "PATCH",
        &format!("/api/files/{}", encode_path(&root.join("c.txt"))),
        Some(r#"{"name":"b.txt"}"#),
        &[],
    )
    .await;
    assert_eq!(conflict.status, StatusCode::CONFLICT);
    assert_eq!(conflict.json()["code"], "already_exists");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_patch_rejects_names_that_contain_paths() {
    let root = fixture_root("renamebad");
    std::fs::write(root.join("a.txt"), "x").unwrap();
    let url = format!("/api/files/{}", encode_path(&root.join("a.txt")));
    for name in ["../x.txt", "sub/x.txt", ".", "..", ""] {
        let body = format!(r#"{{"name":"{name}"}}"#);
        let reply = call(app(&root, &root), "PATCH", &url, Some(&body), &[]).await;
        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "name={name:?}");
        assert_eq!(reply.json()["code"], "invalid_name", "name={name:?}");
    }
    assert!(root.join("a.txt").exists());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_upload_check_reports_conflicts_and_non_replaceable() {
    // 拖拽上传前先查一遍：`POST {fileNames}` → `{conflicts, nonReplaceable}`。
    let root = fixture_root("upcheck");
    std::fs::write(root.join("exists.txt"), "x").unwrap();
    std::fs::create_dir_all(root.join("adir")).unwrap();
    let url = format!("/api/files/{}?type=upload-check", encode_path(&root));

    // GET 带 upload-check 时由 type 校验先挡下（upload 系列只接受 POST 语义）
    let via_get = get(app(&root, &root), &url).await;
    assert_eq!(
        via_get.status,
        StatusCode::BAD_REQUEST,
        "{}",
        via_get.text()
    );

    let reply = call(
        app(&root, &root),
        "POST",
        &url,
        Some(r#"{"fileNames":["exists.txt","adir","new.txt"]}"#),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_eq!(body["conflicts"], json!(["exists.txt", "adir"]));
    assert_eq!(
        body["nonReplaceable"],
        json!(["adir"]),
        "目录不能被上传覆盖"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_upload_check_validates_names() {
    let root = fixture_root("upcheckbad");
    let url = format!("/api/files/{}?type=upload-check", encode_path(&root));

    let empty = call(
        app(&root, &root),
        "POST",
        &url,
        Some(r#"{"fileNames":[]}"#),
        &[],
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST);
    assert!(
        empty.text().contains("No files selected"),
        "{}",
        empty.text()
    );

    let traversal = call(
        app(&root, &root),
        "POST",
        &url,
        Some(r#"{"fileNames":["../evil.txt"]}"#),
        &[],
    )
    .await;
    assert_eq!(traversal.status, StatusCode::BAD_REQUEST);
    assert!(
        traversal.text().contains("must not contain a path"),
        "{}",
        traversal.text()
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_upload_writes_multipart_files() {
    let root = fixture_root("upload");
    let url = format!(
        "/api/files/{}?type=upload&conflict=error",
        encode_path(&root)
    );
    let reply = post_multipart(
        app(&root, &root),
        &url,
        &[
            ("files", "one.txt", b"first"),
            ("files", "two.txt", b"second"),
        ],
    )
    .await;

    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_eq!(body["uploaded"], json!(["one.txt", "two.txt"]));
    assert_eq!(body["skipped"], json!([]));
    assert_eq!(body["errors"], json!([]));
    assert_eq!(
        std::fs::read_to_string(root.join("one.txt")).unwrap(),
        "first"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("two.txt")).unwrap(),
        "second"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn files_upload_honours_the_conflict_strategy() {
    let root = fixture_root("uploadconflict");
    std::fs::write(root.join("a.txt"), "old").unwrap();

    // error：整体 409，且不动已有文件
    let url = format!(
        "/api/files/{}?type=upload&conflict=error",
        encode_path(&root)
    );
    let blocked = post_multipart(app(&root, &root), &url, &[("files", "a.txt", b"new")]).await;
    assert_eq!(blocked.status, StatusCode::CONFLICT);
    assert_eq!(blocked.json()["conflicts"], json!(["a.txt"]));
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "old");

    // skip：跳过，返回 skipped
    let url = format!(
        "/api/files/{}?type=upload&conflict=skip",
        encode_path(&root)
    );
    let skipped = post_multipart(app(&root, &root), &url, &[("files", "a.txt", b"new")]).await;
    assert_eq!(skipped.status, StatusCode::OK);
    assert_eq!(skipped.json()["skipped"], json!(["a.txt"]));
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "old");

    // overwrite：覆盖
    let url = format!(
        "/api/files/{}?type=upload&conflict=overwrite",
        encode_path(&root)
    );
    let replaced = post_multipart(app(&root, &root), &url, &[("files", "a.txt", b"new")]).await;
    assert_eq!(replaced.status, StatusCode::OK);
    assert_eq!(std::fs::read_to_string(root.join("a.txt")).unwrap(), "new");

    // 非法策略
    let url = format!(
        "/api/files/{}?type=upload&conflict=bogus",
        encode_path(&root)
    );
    let bad = post_multipart(app(&root, &root), &url, &[("files", "z.txt", b"x")]).await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST);
    std::fs::remove_dir_all(&root).ok();
}

fn multipart_body(files: &[(&str, &str, &[u8])]) -> (String, Vec<u8>) {
    const BOUNDARY: &str = "XyZzY123";
    let mut out: Vec<u8> = Vec::new();
    for (field, name, data) in files {
        out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
        out.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{field}\"; filename=\"{name}\"\r\n\
                 Content-Type: application/octet-stream\r\n\r\n"
            )
            .as_bytes(),
        );
        out.extend_from_slice(data);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    (BOUNDARY.to_string(), out)
}

async fn post_multipart(router: axum::Router, uri: &str, files: &[(&str, &str, &[u8])]) -> Reply {
    let (boundary, body) = multipart_body(files);
    let request = Request::builder()
        .method("POST")
        .uri(uri)
        .header("host", "127.0.0.1:30141")
        .header(
            "content-type",
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body))
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response
        .into_body()
        .collect()
        .await
        .unwrap()
        .to_bytes()
        .to_vec();
    Reply {
        status,
        headers,
        body,
    }
}

// ============================================================================
// 3) 会话
// ============================================================================

const SESSION_HEADER: &str = r#"{"type":"session","version":3,"id":"sess-a","timestamp":"2026-11-10T06:41:44.431Z","cwd":"/tmp/demo"}"#;

fn write_session(root: &Path, entries: &[&str]) -> PathBuf {
    let dir = root.join("--tmp-demo--");
    std::fs::create_dir_all(&dir).unwrap();
    let mut text = String::from(SESSION_HEADER);
    text.push('\n');
    for entry in entries {
        text.push_str(entry);
        text.push('\n');
    }
    let path = dir.join("2026-11-10T06-41-44-431Z_sess-a.jsonl");
    std::fs::write(&path, text).unwrap();
    path
}

#[tokio::test]
async fn sessions_list_exposes_every_field_the_sidebar_reads() {
    // SessionSidebar 读：name / cwd / modified / messageCount / firstMessage /
    // projectRoot / projectKey / parentSessionId。
    let root = fixture_root("sesslist");
    write_session(
        &root,
        &[
            r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"第一个问题"}],"timestamp":1794292905000}}"#,
            r#"{"type":"session_info","id":"m2","parentId":"m1","timestamp":"2026-11-10T06:41:46.000Z","name":"缓存讨论"}"#,
        ],
    );

    let body = get(app(&root, Path::new("/tmp/demo")), "/api/sessions")
        .await
        .json();
    let sessions = body["sessions"].as_array().unwrap();
    assert_eq!(sessions.len(), 1);
    let item = &sessions[0];
    for field in [
        "path",
        "id",
        "cwd",
        "name",
        "created",
        "modified",
        "messageCount",
        "firstMessage",
        "parentSessionId",
        "transient",
        "projectRoot",
        "projectKey",
    ] {
        assert!(item.get(field).is_some(), "缺少字段 {field}：{item}");
    }
    assert_eq!(item["id"], "sess-a");
    assert_eq!(item["name"], "缓存讨论");
    assert_eq!(item["firstMessage"], "第一个问题");
    assert!(
        body.get("sessionsUpdatedAt").is_some(),
        "客户端用它判新鲜度"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn sessions_detail_carries_tree_context_and_paging() {
    let root = fixture_root("sessdetail");
    let mut entries: Vec<String> = Vec::new();
    for index in 0..5 {
        entries.push(format!(
            r#"{{"type":"message","id":"m{index}","parentId":{},"timestamp":"2026-11-10T06:41:4{index}.000Z","message":{{"role":"user","content":[{{"type":"text","text":"消息 {index}"}}],"timestamp":179429290{index}000}}}}"#,
            if index == 0 {
                "null".to_string()
            } else {
                format!("\"m{}\"", index - 1)
            }
        ));
    }
    let refs: Vec<&str> = entries.iter().map(String::as_str).collect();
    write_session(&root, &refs);

    let body = get(app(&root, Path::new("/tmp/demo")), "/api/sessions/sess-a")
        .await
        .json();
    assert_eq!(body["sessionId"], "sess-a");
    assert!(body["filePath"].as_str().unwrap().ends_with("sess-a.jsonl"));
    assert!(body["info"].is_object());
    assert!(body["leafId"].is_string());
    assert!(body["tree"].is_array());
    assert!(body["context"]["messages"].is_array());
    assert!(body["paging"]["totalMessages"].is_number());
    assert!(body["totalActiveMs"].is_number());

    // limit 夹在 1..400
    let small = get(
        app(&root, Path::new("/tmp/demo")),
        "/api/sessions/sess-a?limit=2",
    )
    .await
    .json();
    let messages = small["context"]["messages"].as_array().unwrap();
    assert!(
        messages.len() <= 2,
        "limit=2 时不该回 {} 条",
        messages.len()
    );
    std::fs::remove_dir_all(&root).ok();
}

/// 「设置里选的默认模型」必须真的生效。
///
/// 阶段 5 续聊走查发现：`LiveAgent::start` 只用了 `catalog.default_model()`
/// （models.json 里第一个可用项），`AppState::default_model_ref()`（读 settings.json）
/// 从没被用上 —— 用户在设置里选的默认模型完全不生效，续聊会用错模型。
#[tokio::test]
async fn default_model_ref_prefers_settings_over_catalog() {
    let root = fixture_root("defmodel");
    // 一个只有两个模型的最小 models.json
    let models = json!({
        "providers": {
            "first": {
                "api": "openai-completions",
                "baseUrl": "https://first.example.test/v1",
                "models": [{"id": "model-a"}]
            },
            "second": {
                "api": "openai-completions",
                "baseUrl": "https://second.example.test/v1",
                "models": [{"id": "model-b"}]
            }
        }
    });
    let catalog = ModelCatalog::from_config(models, None).unwrap();
    let state = AppState::with_cwd(&root, catalog, &root);
    // 不设 settings 时退回 catalog 默认（first/model-a）
    assert_eq!(
        state.default_model_ref(),
        Some(("first".to_string(), "model-a".to_string()))
    );

    // settings.json 指向 second/model-b → 必须跟着变
    let settings_path = root.join("settings.json");
    std::fs::write(
        &settings_path,
        r#"{"defaultProvider":"second","defaultModel":"model-b"}"#,
    )
    .unwrap();
    let state = AppState::with_cwd(&root, ModelCatalog::from_config(json!({
        "providers": {
            "first": {"api": "openai-completions", "baseUrl": "https://first.example.test/v1", "models": [{"id": "model-a"}]},
            "second": {"api": "openai-completions", "baseUrl": "https://second.example.test/v1", "models": [{"id": "model-b"}]}
        }
    }), None).unwrap(), &root)
    .with_stores(
        toiler_server::auth::AuthStore::default(),
        toiler_server::models_json::ModelsFile::default(),
        toiler_server::settings::SettingsFile::at(&settings_path),
        toiler_server::projects::ProjectStore::default(),
        toiler_server::trust::TrustStore::default(),
    );
    assert_eq!(
        state.default_model_ref(),
        Some(("second".to_string(), "model-b".to_string())),
        "settings.json 里的默认模型必须优先于 catalog 的第一个可用项"
    );

    // settings 指向不存在的模型 → 安全退回 catalog 默认
    std::fs::write(
        &settings_path,
        r#"{"defaultProvider":"ghost","defaultModel":"missing"}"#,
    )
    .unwrap();
    let state = AppState::with_cwd(&root, ModelCatalog::from_config(json!({
        "providers": {"first": {"api": "openai-completions", "baseUrl": "https://first.example.test/v1", "models": [{"id": "model-a"}]}}
    }), None).unwrap(), &root)
    .with_stores(
        toiler_server::auth::AuthStore::default(),
        toiler_server::models_json::ModelsFile::default(),
        toiler_server::settings::SettingsFile::at(&settings_path),
        toiler_server::projects::ProjectStore::default(),
        toiler_server::trust::TrustStore::default(),
    );
    assert_eq!(
        state.default_model_ref(),
        Some(("first".to_string(), "model-a".to_string()))
    );
    std::fs::remove_dir_all(&root).ok();
}

/// `with_binding` 必须能在 state 已被 clone 之后调用。
///
/// 阶段 5 真机启动抓到的崩溃：桌面壳先 `state.clone()` 预热会话列表，再
/// `with_binding(...)`，早先用 `Arc::get_mut().expect()` 直接 panic，
/// 应用一启动就 Abort trap。
#[tokio::test]
async fn binding_can_be_set_after_the_state_was_cloned() {
    let root = fixture_root("binding");
    let catalog = ModelCatalog::from_config(json!({ "providers": {} }), None).unwrap();
    let state = AppState::with_cwd(&root, catalog, &root);

    let clone = state.clone(); // 模拟预热任务持有的一份
    let bound = state.with_binding("0.0.0.0", 12345);

    assert_eq!(bound.server_port(), 12345);
    assert_eq!(bound.bind_host(), "0.0.0.0");
    // 共享槽：已经存在的克隆也能看到新值（这正是修复的意义）
    assert_eq!(clone.server_port(), 12345);
    std::fs::remove_dir_all(&root).ok();
}

/// 阶段 5 走查抓到的真实白屏回归：v3 里的 toolCall 块是 `{id, name, arguments}`，
/// 而前端读 `{toolCallId, toolName, input}`。不归一化时 `toolName` 是 undefined，
/// 前端 `isFileWritingToolName(undefined)` 直接 TypeError —— 打开**任何带工具调用**
/// 的历史会话都会整页白屏。
#[tokio::test]
async fn sessions_context_normalizes_tool_call_blocks_for_the_ui() {
    let root = fixture_root("toolnormalize");
    write_session(
        &root,
        &[
            r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"跑个命令"}],"timestamp":1794292905000}}"#,
            r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-11-10T06:41:46.000Z","message":{"role":"assistant","content":[{"type":"toolCall","id":"call_1","name":"bash","arguments":{"command":"echo hi"}}],"api":"openai-responses","provider":"qoder","model":"DeepSeek-Flash","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"toolUse","timestamp":1794292906000}}"#,
            r#"{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-11-10T06:41:47.000Z","message":{"role":"toolResult","toolCallId":"call_1","toolName":"bash","content":[{"type":"text","text":"hi"}]}}"#,
        ],
    );

    let body = get(app(&root, Path::new("/tmp/demo")), "/api/sessions/sess-a")
        .await
        .json();
    let messages = body["context"]["messages"].as_array().unwrap();
    let assistant = messages
        .iter()
        .find(|m| m["role"] == "assistant")
        .expect("assistant 消息应在 context 里");
    let block = &assistant["content"][0];
    assert_eq!(block["type"], "toolCall");
    assert_eq!(block["toolCallId"], "call_1", "前端读 toolCallId：{block}");
    assert_eq!(block["toolName"], "bash", "前端读 toolName：{block}");
    assert_eq!(
        block["input"]["command"], "echo hi",
        "前端读 input：{block}"
    );

    // toolResult 本来就是 toolName，不该被改名
    let result = messages
        .iter()
        .find(|m| m["role"] == "toolResult")
        .expect("toolResult 应在 context 里");
    assert_eq!(result["toolName"], "bash");
    assert_eq!(result["toolCallId"], "call_1");
    std::fs::remove_dir_all(&root).ok();
}

/// 同一处回归的另一半：`role: "system"` 的转录消息（prompt / 工具清单）不进 context。
#[tokio::test]
async fn sessions_context_skips_system_messages() {
    let root = fixture_root("sysmsgs");
    write_session(
        &root,
        &[
            r#"{"type":"message","id":"m0","parentId":null,"timestamp":"2026-11-10T06:41:44.000Z","message":{"role":"system","content":[{"type":"text","text":"you are pi"}],"timestamp":1794292904000}}"#,
            r#"{"type":"message","id":"m1","parentId":"m0","timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":1794292905000}}"#,
        ],
    );
    let body = get(app(&root, Path::new("/tmp/demo")), "/api/sessions/sess-a")
        .await
        .json();
    let messages = body["context"]["messages"].as_array().unwrap();
    assert!(
        messages.iter().all(|m| m["role"] != "system"),
        "system 消息不该出现在 context 里：{messages:?}"
    );
    assert_eq!(messages.len(), 1);
    std::fs::remove_dir_all(&root).ok();
}

/// `deferThinking` 要把思考正文清空并标记 `deferred`（前端据此显示「展开思考」）。
#[tokio::test]
async fn sessions_defer_thinking_clears_and_marks_blocks() {
    let root = fixture_root("deferthinking");
    write_session(
        &root,
        &[
            r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"想一下"}],"timestamp":1794292905000}}"#,
            r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-11-10T06:41:46.000Z","message":{"role":"assistant","content":[{"type":"thinking","thinking":"很长的推理"},{"type":"text","text":"答案"}],"api":"openai-responses","provider":"qoder","model":"x","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1794292906000}}"#,
        ],
    );

    let plain = get(app(&root, Path::new("/tmp/demo")), "/api/sessions/sess-a")
        .await
        .json();
    let assistant = plain["context"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap()
        .clone();
    assert_eq!(
        assistant["content"][0]["thinking"], "很长的推理",
        "默认不裁剪"
    );
    assert!(assistant["content"][0].get("deferred").is_none());

    let deferred = get(
        app(&root, Path::new("/tmp/demo")),
        "/api/sessions/sess-a?deferThinking=1",
    )
    .await
    .json();
    let assistant = deferred["context"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["role"] == "assistant")
        .unwrap()
        .clone();
    assert_eq!(assistant["content"][0]["thinking"], "");
    assert_eq!(assistant["content"][0]["deferred"], true);
    assert_eq!(assistant["content"][1]["text"], "答案", "正文不受影响");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn sessions_rename_appends_a_session_info_entry() {
    // PATCH 改名走的是「追加 session_info」而不是改 header（pi 的做法）。
    let root = fixture_root("sessrename");
    let path = write_session(
        &root,
        &[
            r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":1794292905000}}"#,
        ],
    );
    let before = std::fs::read_to_string(&path).unwrap();

    let reply = call(
        app(&root, Path::new("/tmp/demo")),
        "PATCH",
        "/api/sessions/sess-a",
        Some(r#"{"name":"新名字"}"#),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());

    let after = std::fs::read_to_string(&path).unwrap();
    assert!(
        after.starts_with(&before),
        "必须 append-only，老字节一字不改"
    );
    assert!(after.contains("session_info"), "{after}");
    assert!(after.contains("新名字"));

    let detail = get(app(&root, Path::new("/tmp/demo")), "/api/sessions/sess-a")
        .await
        .json();
    assert_eq!(detail["info"]["name"], "新名字");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn sessions_search_finds_text_in_bodies() {
    let root = fixture_root("sesssearch");
    write_session(
        &root,
        &[
            r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"用 Redis 做缓存"}],"timestamp":1794292905000}}"#,
        ],
    );
    let body = get(
        app(&root, Path::new("/tmp/demo")),
        "/api/sessions/search?q=Redis",
    )
    .await
    .json();
    let results = body["matches"].as_array().unwrap();
    assert_eq!(results.len(), 1, "{body}");
    assert_eq!(results[0]["sessionId"], "sess-a");
    assert!(results[0]["snippet"].as_str().unwrap().contains("Redis"));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn sessions_delete_removes_the_file() {
    let root = fixture_root("sessdelete");
    let path = write_session(
        &root,
        &[
            r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"hi"}],"timestamp":1794292905000}}"#,
        ],
    );
    let reply = call(
        app(&root, Path::new("/tmp/demo")),
        "DELETE",
        "/api/sessions/sess-a",
        None,
        &[],
    )
    .await;
    assert!(reply.status.is_success(), "{}", reply.text());
    assert!(!path.exists());
    std::fs::remove_dir_all(&root).ok();
}

// ============================================================================
// 4) 其它首屏 route
// ============================================================================

#[tokio::test]
async fn home_and_default_cwd_answer_the_shapes_the_shell_needs() {
    let root = fixture_root("home");
    let body = get(app(&root, &root), "/api/home").await.json();
    assert!(body["home"].as_str().is_some());

    let reply = call(
        app(&root, &root),
        "POST",
        "/api/default-cwd",
        Some("{}"),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert!(reply.json()["cwd"].as_str().is_some());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn tools_settings_hides_the_powershell_group_on_non_windows() {
    // 前端 SettingsPanel 依赖：`GET /api/tools/settings` 只要**失败**（原先是缺路由
    // → 404），常规页就会渲染「Shell 工具」这一组，并把 error 原文当红字显示出来
    // （就是设置页里那条莫名其妙的 "Not found"）。`isWindows === false` 时整组不渲染。
    let root = fixture_root("tools-settings");
    let reply = get(app(&root, &root), "/api/tools/settings").await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_eq!(body["isWindows"], cfg!(windows));
    assert_eq!(
        body["powerShellEnabled"], false,
        "toiler 的 shell 工具是 bash，没有 PowerShell 通道"
    );

    // PUT 只在 Windows 的 pi 上有意义，这里给明确错误而不是静默成功。
    let put = call(
        app(&root, &root),
        "PUT",
        "/api/tools/settings",
        Some("{\"enabled\":true}"),
        &[],
    )
    .await;
    assert_eq!(put.status, StatusCode::NOT_IMPLEMENTED, "{}", put.text());
}

#[tokio::test]
async fn queue_modes_route_round_trips_and_never_404s() {
    // 前端 `SettingsPanel` 一挂载就 `GET /api/queue-modes`，**只要失败**（原先缺路由
    // → 静态兜底的 `{"error":"Not found"}`），它就把 error 原文画成「聊天」分组
    // 末尾的一行红字 —— 位置正好在「完成提示音」下面，就是用户看到的那条 "Not found"。
    let root = fixture_root("queue-modes");
    let agent_dir = fixture_root("queue-modes-agent");

    let reply = get(
        app_with_agent_dir(&root, &root, Some(&agent_dir)),
        "/api/queue-modes",
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    // 没配过 = pi 的默认值（一条一条投），不是「未配置」，更不该是 404。
    assert_eq!(body["steeringMode"], "one-at-a-time");
    assert_eq!(body["followUpMode"], "one-at-a-time");

    // 两个都给：返回写盘后的完整一对。
    let put = call(
        app_with_agent_dir(&root, &root, Some(&agent_dir)),
        "PUT",
        "/api/queue-modes",
        Some(r#"{"steeringMode":"all","followUpMode":"all"}"#),
        &[],
    )
    .await;
    assert_eq!(put.status, StatusCode::OK, "{}", put.text());
    assert_eq!(put.json()["steeringMode"], "all");
    assert_eq!(put.json()["followUpMode"], "all");

    // rpi 的队列模式在 `AgentHarness::create` 时就定下来（没有运行期 setter），
    // 所以这份文件是「新会话按什么模式投递」的唯一依据，必须真的落盘。
    let text = std::fs::read_to_string(agent_dir.join("settings.json")).unwrap();
    assert!(text.contains("\"steeringMode\": \"all\""), "{text}");
    assert!(text.contains("\"followUpMode\": \"all\""), "{text}");

    // 只给一个：另一个保持原值（不是被重置成默认）。
    let partial = call(
        app_with_agent_dir(&root, &root, Some(&agent_dir)),
        "PUT",
        "/api/queue-modes",
        Some(r#"{"followUpMode":"one-at-a-time"}"#),
        &[],
    )
    .await;
    assert_eq!(partial.status, StatusCode::OK, "{}", partial.text());
    assert_eq!(partial.json()["steeringMode"], "all");
    assert_eq!(partial.json()["followUpMode"], "one-at-a-time");

    // 非法值与「什么都不给」：400 + 可读原因。前端会把这条画成红字，不能是空串。
    let bad = call(
        app_with_agent_dir(&root, &root, Some(&agent_dir)),
        "PUT",
        "/api/queue-modes",
        Some(r#"{"steeringMode":"sometimes"}"#),
        &[],
    )
    .await;
    assert_eq!(bad.status, StatusCode::BAD_REQUEST, "{}", bad.text());
    assert!(
        bad.json()["error"]
            .as_str()
            .is_some_and(|e| e.contains("steeringMode")),
        "{}",
        bad.text()
    );

    let empty = call(
        app_with_agent_dir(&root, &root, Some(&agent_dir)),
        "PUT",
        "/api/queue-modes",
        Some("{}"),
        &[],
    )
    .await;
    assert_eq!(empty.status, StatusCode::BAD_REQUEST, "{}", empty.text());

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
}

#[tokio::test]
async fn cwd_validate_returns_project_identity() {
    // 前端的项目切换器直接用这三个字段建项目条目。
    let root = fixture_root("cwdvalidate");
    let agent_dir = fixture_root("cwdvalidate-agent");
    let project = root.join("proj");
    std::fs::create_dir_all(&project).unwrap();
    let body = call(
        app_with_agent_dir(&root, &root, Some(&agent_dir)),
        "POST",
        "/api/cwd/validate",
        Some(&format!(r#"{{"cwd":"{}"}}"#, project.to_string_lossy())),
        &[],
    )
    .await
    .json();
    assert_eq!(body["success"], true);
    assert_eq!(body["cwd"], project.to_string_lossy().as_ref());
    assert!(body["projectRoot"].as_str().is_some());
    assert!(body["projectKey"].as_str().is_some());

    // 项目必须登记到**隔离库**里，而不是用户的 ~/.agents/toiler/work.sqlite
    assert!(agent_dir.join("work.sqlite").exists(), "应写进隔离的项目库");
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
}

#[tokio::test]
async fn cwd_browse_lists_directories_only() {
    let root = fixture_root("cwdbrowse");
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("file.txt"), "x").unwrap();

    let url = format!(
        "/api/cwd/browse?path={}",
        url_encode(&root.to_string_lossy())
    );
    let body = get(app(&root, &root), &url).await.json();
    let names: Vec<&str> = body["directories"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|entry| entry["name"].as_str().or_else(|| entry.as_str()))
        .collect();
    assert!(names.contains(&"sub"), "{body}");
    assert!(!names.contains(&"file.txt"), "浏览目录不该列文件：{body}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn models_exposes_provider_and_id_for_the_picker() {
    // 对应前端 `useAgentSession.ts:loadModels()` + `ChatInput.tsx:modelOptions`：
    // 它们读的是 `modelList`（数组）与 `models`（`Record<string,string>`）。
    // 两者必须同时存在且形状正确，否则下拉框渲染会在排序时抛错、整页白屏。
    let root = fixture_root("models");
    let body = get(app(&root, &root), "/api/models").await.json();
    assert!(body["models"].is_object(), "models 必须是字典：{body}");
    assert!(body["modelList"].is_array(), "modelList 必须是数组：{body}");
    assert!(body.get("source").is_some());
    assert!(body["defaultModel"].is_null() || body["defaultModel"].is_object());
    assert!(body["thinkingLevels"].is_object());
    assert!(body["thinkingLevelMaps"].is_object());
    assert!(body["thinkingLevelPins"].is_object());
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn models_dict_and_list_share_the_same_keys() {
    // 对应前端 `ChatInput.tsx:modelOptions`：它优先用 `modelList`，拿不到就回退
    // 到 `Object.entries(models)`。两条路径必须给出同一批模型，否则下拉框会
    // 随响应形式忽多忽少（特别是 `models` 是数组时就完全错乱了）。
    let root = fixture_root("models2");
    let body = get(app(&root, &root), "/api/models").await.json();
    let dict = body["models"].as_object().expect("models 必须是字典");
    let list = body["modelList"].as_array().expect("modelList 必须是数组");
    assert_eq!(dict.len(), list.len(), "{body}");
    for item in list {
        let provider = item["provider"].as_str().unwrap();
        let id = item["id"].as_str().unwrap();
        // 键是 `provider:id`（**冒号**），不是 `provider/id`。
        let key = format!("{provider}:{id}");
        assert!(dict.contains_key(&key), "缺字典键 {key}：{body}");
        assert_eq!(dict[&key], item["name"], "两个字典的显示名不一致");
    }
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn auth_provider_lists_come_from_the_builtin_catalog() {
    // 对应设置页「模型 → 添加模型 → 供应商」面板（`components/ModelsConfig.tsx`
    // 里的 `AddProviderPicker` 与 `loadOAuthProviders` / `loadApiKeyProviders`）：
    //   * 「订阅」区   ← `GET /api/auth/providers`（pi 的 `buildOAuthProviderList`）
    //   * 「API Key」区 ← `GET /api/auth/all-providers`（pi 的 `buildApiKeyProviderList`）
    //
    // 阶段 5 之前：前一个只遍历 `models.json`（用户在里边写全 `apiKey` 时就是空
    // 数组），后一个恒返回 `[]` —— 面板里只剩「OpenAI / Anthropic compatible」
    // 一张自定义卡片。现在供应商来自写死的内置表（`crate::builtin_providers`，
    // pi SDK 的 42 家 provider 目录）。
    let root = fixture_root("auth-providers");
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    let router = app_with_agent_dir(&root, &root, Some(&agent));

    let api_key = get(router.clone(), "/api/auth/all-providers").await.json();
    let api_key_rows = api_key["providers"]
        .as_array()
        .expect("providers 必须是数组");
    let mut api_key_ids: Vec<&str> = api_key_rows
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    api_key_ids.sort_unstable();
    // 42 家里只有 openai-codex 不声明 API Key 登录；加上 pi-desktop 自注册的
    // `apisets`（每家都声明 API Key）共 42。
    assert_eq!(api_key_ids.len(), 42, "{api_key}");
    assert!(api_key_ids.contains(&"anthropic"));
    assert!(api_key_ids.contains(&"deepseek"));
    assert!(api_key_ids.contains(&"apisets"));
    assert!(api_key_ids.contains(&"zai"));
    assert!(!api_key_ids.contains(&"openai-codex"));
    assert!(!api_key_ids.contains(&"qoder-cn"));

    let anthropic = api_key_rows
        .iter()
        .find(|row| row["id"] == "anthropic")
        .unwrap();
    assert_eq!(anthropic["displayName"], "Anthropic");
    assert_eq!(anthropic["baseUrl"], "https://api.anthropic.com");
    assert_eq!(anthropic["configured"], false);
    assert_eq!(anthropic["modelCount"], 17);
    assert_eq!(anthropic["supportsOAuth"], true);

    let oauth = get(router, "/api/auth/providers").await.json();
    let oauth_rows = oauth["providers"].as_array().expect("providers 必须是数组");
    let mut oauth_ids: Vec<&str> = oauth_rows
        .iter()
        .map(|row| row["id"].as_str().unwrap())
        .collect();
    oauth_ids.sort_unstable();
    assert_eq!(
        oauth_ids,
        vec![
            "anthropic",
            "github-copilot",
            "kimi-coding",
            "meta",
            "openai",
            "openai-codex",
            "openrouter",
            "qoder-cn",
            "radius",
            "xai",
        ],
        "{oauth}"
    );
    // pi-desktop 自注册的订阅家：卡片标题取 `oauth.name`
    let qoder = oauth_rows
        .iter()
        .find(|row| row["id"] == "qoder-cn")
        .unwrap();
    assert_eq!(qoder["name"], "Qoder CN (subscription)");
    assert_eq!(qoder["supportsApiKey"], false);
    // 面板卡片标题取的是 OAuth 名（不是 provider.name），openai-codex 被 pi 的
    // `OAUTH_DISPLAY_NAMES` 改成 `ChatGPT Plus/Pro`。
    let codex = oauth_rows
        .iter()
        .find(|row| row["id"] == "openai-codex")
        .unwrap();
    assert_eq!(codex["name"], "ChatGPT Plus/Pro");
    assert_eq!(codex["supportsApiKey"], false);
    assert_eq!(codex["loggedIn"], false);

    // 面板总数：42（API Key）+ 10（订阅）+ 1（自定义卡片）= pi 的 53 张。
    assert_eq!(api_key_rows.len() + oauth_rows.len() + 1, 53);
    std::fs::remove_dir_all(&root).ok();
}

/// 建一个隔离的 `AppState`（`qoder-cn` 的用例要拿目录与令牌，不只是 Router）。
fn state_with_agent_dir(sessions_root: &Path, cwd: &Path, agent_dir: &Path) -> AppState {
    let catalog = ModelCatalog::from_config(json!({ "providers": {} }), None).unwrap();
    AppState::with_cwd(sessions_root, catalog, cwd)
        .with_stores(
            toiler_server::auth::AuthStore::new(agent_dir.join("auth.json")),
            toiler_server::models_json::ModelsFile::at(agent_dir.join("models.json")),
            toiler_server::settings::SettingsFile::at(agent_dir.join("settings.json")),
            toiler_server::projects::ProjectStore::at(agent_dir.join("work.sqlite")),
            toiler_server::trust::TrustStore::at(agent_dir.join("trust.json")),
        )
        .with_binding("127.0.0.1", 30141)
}

#[tokio::test]
async fn qoder_cn_relay_requires_the_token_and_a_login() {
    // 对应 `qoder_cn::proxy`：rpi 的 provider 只能按 baseUrl 发标准 OpenAI 请求，
    // 所以 `qoder-cn` 的模型 baseUrl 指向本进程的
    // `POST /qoder-cn/{token}/v1/chat/completions`。这条端点会带着账号凭据刷上游
    // 额度，因此两件事必须成立：
    //   1. 路径令牌对不上 → 403（令牌是进程内随机串，只有内嵌目录知道）；
    //   2. 没登录 → 502 + 中文提示（前端把这条消息原样显示给用户）。
    let root = fixture_root("qoder-relay");
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    let state = state_with_agent_dir(&root, &root, &agent);
    let token = state.qoder_cn_token();
    let router = routes::router(state);

    let body = json!({
        "model": "gfmodel",
        "messages": [{"role": "user", "content": "hi"}],
    })
    .to_string();

    // 令牌不匹配
    let reply = call(
        router.clone(),
        "POST",
        "/qoder-cn/not-the-token/v1/chat/completions",
        Some(&body),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::FORBIDDEN, "{}", reply.text());

    // 令牌正确但 auth.json 里没有 qoder-cn 凭据
    let reply = call(
        router,
        "POST",
        &format!("/qoder-cn/{token}/v1/chat/completions"),
        Some(&body),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::BAD_GATEWAY, "{}", reply.text());
    let payload = reply.json();
    let message = payload["error"]["message"].as_str().unwrap_or("");
    assert!(message.contains("未登录"), "{payload}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn qoder_cn_models_come_from_the_builtin_catalog() {
    // 对应设置页「模型」下拉与模型设置列表：`qoder-cn` 的 15 个模型来自内置静态
    // 目录（pi 的 `QODER_CN_STATIC_CATALOG`），`baseUrl` 指向本机转发端点。
    let root = fixture_root("qoder-models");
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    let state = state_with_agent_dir(&root, &root, &agent);
    let token = state.qoder_cn_token();
    assert!(state.attach_qoder_cn(), "第一次注入应生效");
    assert!(!state.attach_qoder_cn(), "重复注入应幂等");

    let catalog = state.catalog();
    let qoder: Vec<&toiler_server::catalog::ModelEntry> = catalog
        .entries
        .iter()
        .filter(|entry| entry.provider == "qoder-cn")
        .collect();
    assert_eq!(qoder.len(), 15, "内置目录 15 条");
    assert_eq!(qoder[0].id, "auto");
    assert_eq!(qoder[0].context_window, 200_000);
    let model = catalog
        .models
        .iter()
        .find(|model| model.id == "auto")
        .expect("rpi 模型");
    assert_eq!(
        model.base_url,
        format!("http://127.0.0.1:30141/qoder-cn/{token}")
    );
    std::fs::remove_dir_all(&root).ok();
}

/// `apisets`（设置页里的 **Coding**）的一套缓存 + 凭据。
///
/// 形状逐字对齐 pi 写的 `models-store.json`：`models` 里的每一条都带 `provider` /
/// `api` / `baseUrl`（恢复缓存时要按 `provider` 过滤，`api` 决定走哪套线协议）。
fn seed_apisets(agent: &Path, models: &[(&str, &str)], checked_at: i64) {
    let defs: Vec<Value> = models
        .iter()
        .map(|(id, api)| {
            json!({
                "id": id,
                "name": id.to_uppercase(),
                "provider": "apisets",
                "api": api,
                "baseUrl": "https://coding.apisets.com",
                "reasoning": true,
                "input": ["text", "image"],
                "cost": { "input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0 },
                "contextWindow": 1_048_576,
                "maxTokens": 131_072,
            })
        })
        .collect();
    std::fs::write(
        agent.join("models-store.json"),
        json!({
            // pi 写的另一家：读的时候不能把它当成我们的
            "deepseek": { "models": [], "checkedAt": 1, "etag": "models.dev" },
            "apisets": {
                "models": defs,
                "checkedAt": checked_at,
                "etag": "apisets-models-api-v2",
            },
        })
        .to_string(),
    )
    .unwrap();
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[tokio::test]
async fn apisets_models_come_from_the_remote_catalog_cache() {
    // 对应真实 bug：设置 → 模型 → Coding（`apisets`）显示「该服务商还没有已知模型」，
    // 旁边还挂着 8 条 `No models match pattern "apisets/…"`。
    //
    // 根因是 toiler 只把 `apisets` 当「API Key 服务商」写进了内置表（`models: &[]`），
    // 却没实现 pi 的远端目录（缓存 `models-store.json` + 上游 `/v1/models`）。
    // 这里锁住三件事：缓存能读进来、目录能注入、白名单不再报假警告。
    let root = fixture_root("apisets-catalog");
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    seed_apisets(
        &agent,
        &[
            ("glm-5.3", "anthropic-messages"),
            ("gpt-6-sol", "openai-responses"),
        ],
        now_ms(),
    );
    std::fs::write(
        agent.join("auth.json"),
        r#"{"apisets":{"type":"api_key","key":"sk-test"}}"#,
    )
    .unwrap();
    std::fs::write(
        agent.join("settings.json"),
        r#"{"enabledModels":["apisets/glm-5.3","apisets/gpt-6-sol"]}"#,
    )
    .unwrap();

    let state = state_with_agent_dir(&root, &root, &agent);
    assert!(state.attach_apisets(), "有缓存 + 有 key 时应当注入目录");
    // 幂等，且刷新后旧清单要被替换而不是叠加
    assert!(state.attach_apisets());

    let catalog = state.catalog();
    let entries: Vec<&toiler_server::catalog::ModelEntry> = catalog
        .entries
        .iter()
        .filter(|entry| entry.provider == "apisets")
        .collect();
    assert_eq!(entries.len(), 2, "两次注入不能变成 4 条：{entries:?}");
    // 两套协议各自一个 api，不能都落成同一个
    let apis: Vec<&str> = entries.iter().map(|entry| entry.api.as_str()).collect();
    assert!(apis.contains(&"anthropic-messages"));
    assert!(apis.contains(&"openai-responses"));
    assert_eq!(
        catalog
            .models
            .iter()
            .filter(|model| model.provider.as_str() == "apisets")
            .count(),
        2
    );
    assert!(catalog.providers.iter().any(|p| p.id() == "apisets"));

    // 设置页那个「模型选择列表」：现在应当列出 2 个且都能勾
    let body = get(
        routes::router(state.clone()),
        "/api/models-config/enabled?provider=apisets",
    )
    .await
    .json();
    let models = body["models"].as_array().expect("models 必须是数组");
    assert_eq!(models.len(), 2, "{body}");
    // 显示名取目录里的（pi 用 model.name），不是 id
    assert!(
        models
            .iter()
            .any(|row| row["name"] == "GLM-5.3" || row["name"] == "GPT-6-SOL"),
        "面板上的显示名应当来自目录：{body}"
    );
    assert_eq!(body["refreshing"], false, "有缓存时不该再说「正在拉」");
    assert_eq!(
        body["warnings"].as_array().map(Vec::len),
        Some(0),
        "白名单里的 apisets/* 现在都能匹配上，不该再有 no-match 警告：{body}"
    );
    assert!(
        models.iter().all(|row| row["selected"] == true),
        "enabledModels 里的两条都该是勾上的：{body}"
    );

    // 卡片上的「N models」也不能还是 0
    let listing = get(routes::router(state.clone()), "/api/auth/all-providers")
        .await
        .json();
    let card = listing["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["id"] == "apisets")
        .expect("apisets 卡片");
    assert_eq!(card["displayName"], "Coding");
    assert_eq!(card["modelCount"], 2, "{card}");

    // 删掉 API Key → 模型不该再挂在目录里（不然模型下拉里会有发不出去的模型）
    let reply = call(
        routes::router(state.clone()),
        "DELETE",
        "/api/auth/api-key/apisets",
        None,
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    assert!(
        state
            .catalog()
            .entries
            .iter()
            .all(|entry| entry.provider != "apisets"),
        "key 没了，目录里的 apisets 模型也该没"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn apisets_without_key_or_cache_stays_empty_instead_of_inventing_models() {
    // 没 key：既不能注入目录，也不能联网（`spawn_apisets_refresh` 直接放弃）。
    // 设置页这时候显示「该服务商还没有已知模型」是**对的** —— 用户要先去填 key。
    let root = fixture_root("apisets-empty");
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    seed_apisets(&agent, &[("glm-5.3", "anthropic-messages")], now_ms());
    // 只有缓存、没有 auth.json
    let state = state_with_agent_dir(&root, &root, &agent);
    assert!(!state.attach_apisets(), "没 key 时不该注入");
    assert!(!state.spawn_apisets_refresh(), "没 key 时不该发网络请求");

    let body = get(
        routes::router(state.clone()),
        "/api/models-config/enabled?provider=apisets",
    )
    .await
    .json();
    assert_eq!(body["models"].as_array().map(Vec::len), Some(0), "{body}");
    // 前端靠 `refreshing` 决定要不要自动重试（`EnabledModelsPicker` 的 1.5s ×2）
    assert_eq!(body["refreshing"], true, "{body}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn apisets_cache_from_an_older_etag_is_ignored() {
    // pi 的常量是 `apisets-models-api-v2`，对不上就整份作废（v1 只有 chat 之外的
    // 半截数据）。这里即使有 key，也不能把旧格式的条目当成目录。
    let root = fixture_root("apisets-etag");
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("models-store.json"),
        json!({
            "apisets": {
                "models": [{ "id": "glm-5.3", "provider": "apisets", "api": "anthropic-messages" }],
                "checkedAt": now_ms(),
                "etag": "apisets-models-api-v1",
            }
        })
        .to_string(),
    )
    .unwrap();
    std::fs::write(
        agent.join("auth.json"),
        r#"{"apisets":{"type":"api_key","key":"sk-test"}}"#,
    )
    .unwrap();

    let state = state_with_agent_dir(&root, &root, &agent);
    assert!(!state.attach_apisets(), "旧 etag 的缓存必须整份作废");
    assert!(state
        .catalog()
        .entries
        .iter()
        .all(|entry| entry.provider != "apisets"));
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn apisets_yields_to_a_user_defined_models_json_provider() {
    // 用户自己在 `models.json` 里写了 `apisets`（指向自己的代理）时，内置目录不能
    // 插手 —— 与 `qoder-cn` 同一条规则。
    let root = fixture_root("apisets-override");
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    seed_apisets(&agent, &[("glm-5.3", "anthropic-messages")], now_ms());
    std::fs::write(
        agent.join("auth.json"),
        r#"{"apisets":{"type":"api_key","key":"sk-test"}}"#,
    )
    .unwrap();
    std::fs::write(
        agent.join("models.json"),
        r#"{"providers":{"apisets":{"api":"anthropic-messages","baseUrl":"https://my-proxy.test","apiKey":"k","models":[{"id":"glm-5.3"}]}}}"#,
    )
    .unwrap();

    let state = state_with_agent_dir(&root, &root, &agent);
    assert!(
        !state.attach_apisets(),
        "用户自己定义了同名 provider 时不注入"
    );
    assert!(!state.warm_apisets_catalog(), "也不该擅自联网");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn models_config_enabled_warnings_are_strings() {
    // 对应设置面板的模型页（`ModelSettings`）对 `warnings` 的渲染：pi 的
    // `/api/models-config/enabled` 里取的是 `resolveModelScopeWithDiagnostics`
    // 的 `diagnostics.map(d => d.message)`，即 **string[]**，前端逐条当文本插进 JSX。
    // 返回 `{type,code,message,pattern}` 对象时 React 抛 #31
    // （Objects are not valid as a React child），整个设置页落到
    // “This page couldn't load” 兜底页 —— 真实用户看到的就是这一页。
    let root = fixture_root("models-enabled");
    let agent = root.join("agent");
    std::fs::create_dir_all(&agent).unwrap();
    std::fs::write(
        agent.join("models.json"),
        r#"{"providers":{
            "OpenAI":{"api":"openai-completions","baseUrl":"https://a.example.test/v1","apiKey":"k","models":[{"id":"gpt-5.4"}]},
            "other":{"api":"openai-completions","baseUrl":"https://b.example.test/v1","apiKey":"k","models":[{"id":"gpt-5.4"}]}
        }}"#,
    )
    .unwrap();
    // 一条匹配不上（no-match），一条不带 provider 却命中两个 provider（ambiguous）。
    std::fs::write(
        agent.join("settings.json"),
        r#"{"enabledModels":["ghost/nope","gpt-5.4"]}"#,
    )
    .unwrap();

    let body = get(
        app_with_agent_dir(&root, &root, Some(&agent)),
        "/api/models-config/enabled?provider=OpenAI",
    )
    .await
    .json();
    assert_eq!(body["provider"], "OpenAI");
    let warnings = body["warnings"].as_array().expect("warnings 必须是数组");
    assert_eq!(
        warnings.len(),
        2,
        "应当有 no-match 与 ambiguous 各一条：{body}"
    );
    for warning in warnings {
        assert!(
            warning.is_string(),
            "warnings 元素必须是字符串，前端会直接当文本渲染：{warning}"
        );
    }
    let texts: Vec<&str> = warnings.iter().filter_map(|w| w.as_str()).collect();
    assert!(
        texts.iter().any(|t| t.contains("ghost/nope")),
        "缺少 no-match 文案：{body}"
    );
    assert!(
        texts.iter().any(|t| t.contains("several providers")),
        "缺少 ambiguous 文案：{body}"
    );
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn thinking_levels_match_pi_semantics() {
    // 直接锁住 pi 的 `getSupportedThinkingLevels` 三条规则，避免以后有人
    // 换成 rpi 的 `supported_thinking_levels()`：那个函数对非推理模型也会
    // 返回 off/minimal/low/medium/high，前端会在不支持推理的模型上显示出
    // 推理强度菜单。
    use rpi_ai::{Api, Model, ThinkingLevel};
    use toiler_server::catalog::supported_thinking_levels;

    let mut model = Model::new("m", "M", Api::OpenaiCompletions, "p", "http://x");
    model.reasoning = false;
    assert_eq!(supported_thinking_levels(&model), vec!["off"]);

    model.reasoning = true;
    assert_eq!(
        supported_thinking_levels(&model),
        vec!["off", "minimal", "low", "medium", "high"],
        "缺省不给 xhigh/max"
    );

    // `null` = 显式不支持
    model.thinking_level_map = Some(BTreeMap::from([
        (ThinkingLevel::Minimal, None),
        (ThinkingLevel::Medium, None),
    ]));
    assert_eq!(
        supported_thinking_levels(&model),
        vec!["off", "low", "high"]
    );

    // `max` 映射到非 null 才算支持
    model.thinking_level_map = Some(BTreeMap::from([(
        ThinkingLevel::Max,
        Some("max".to_string()),
    )]));
    assert_eq!(
        supported_thinking_levels(&model),
        vec!["off", "minimal", "low", "medium", "high", "max"]
    );
}

#[tokio::test]
async fn agent_state_for_unknown_session_reports_not_running() {
    let root = fixture_root("agentstate");
    let body = get(app(&root, &root), "/api/agent/nope").await.json();
    assert_eq!(body["running"], false, "{body}");
    std::fs::remove_dir_all(&root).ok();
}

#[tokio::test]
async fn agent_running_lists_both_collections() {
    let root = fixture_root("agentrunning");
    let body = get(app(&root, &root), "/api/agent/running").await.json();
    assert!(body["runningSessionIds"].is_array());
    assert!(body["pendingUiSessionIds"].is_array());
    std::fs::remove_dir_all(&root).ok();
}

/// 带两个可用模型的 Router。`set_model` 要能通过 `catalog.model_for` 查到目标。
fn app_with_two_models(sessions_root: &Path, cwd: &Path, agent_dir: &Path) -> axum::Router {
    let catalog = ModelCatalog::from_config(
        json!({
            "providers": {
                "first": {
                    "api": "openai-completions",
                    "baseUrl": "https://first.example.test/v1",
                    "apiKey": "test-key",
                    "models": [{ "id": "model-a" }]
                },
                "second": {
                    "api": "openai-completions",
                    "baseUrl": "https://second.example.test/v1",
                    "apiKey": "test-key",
                    "models": [{ "id": "model-b" }]
                }
            }
        }),
        None,
    )
    .unwrap();
    let state = AppState::with_cwd(sessions_root, catalog, cwd)
        .with_binding("127.0.0.1", 30141)
        .with_stores(
            toiler_server::auth::AuthStore::new(agent_dir.join("auth.json")),
            toiler_server::models_json::ModelsFile::at(agent_dir.join("models.json")),
            toiler_server::settings::SettingsFile::at(agent_dir.join("settings.json")),
            toiler_server::projects::ProjectStore::at(agent_dir.join("work.sqlite")),
            toiler_server::trust::TrustStore::at(agent_dir.join("trust.json")),
        );
    routes::router(state)
}

/// `ModelPicker` 在**已有会话**里切模型 → `POST /api/agent/{id} {"type":"set_model"}`。
///
/// 这条命令以前没实现，落到 `send` 的兜底分支报「不支持的 agent 命令：set_model」。
/// 新会话（草稿）那条前端路径把错误吞进 `console.error`，所以只有「会话中切换」
/// 会弹红色提示 —— 这正是用户报的必现条件。
#[tokio::test]
async fn set_model_on_existing_session_persists_model_change() {
    let root = fixture_root("setmodel");
    let agent_dir = fixture_root("setmodel-agent");
    let path = write_session(&root, &[]);
    let cwd = Path::new("/tmp/demo");
    let router = app_with_two_models(&root, cwd, &agent_dir);

    let reply = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"set_model","provider":"second","modelId":"model-b"}"#),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(body["data"]["id"], "model-b", "{body}");
    assert_eq!(body["data"]["provider"], "second", "{body}");

    // 前端切完立刻 `loadSession` 读 `/api/sessions/{id}`，`context.model` 必须
    // 已经是新模型（只改内存、不落 model_change 的话这里会弹回旧值）。
    let session = get(router.clone(), "/api/sessions/sess-a").await.json();
    assert_eq!(
        session["context"]["model"]["provider"], "second",
        "{session}"
    );
    assert_eq!(
        session["context"]["model"]["modelId"], "model-b",
        "{session}"
    );

    // `GET /api/agent/{id}` 只在流式中回 `state`（pi 的契约），所以模型是否生效
    // 看上面 `/api/sessions` 的 `context.model`。

    // 会话文件尾部追加 `model_change`：前端切完 loadSession 读的就是它，
    // 不写的话界面会弹回旧模型。
    let text = std::fs::read_to_string(&path).unwrap();
    let last = text.lines().last().expect("会话文件不该是空的");
    let entry: Value = serde_json::from_str(last).unwrap();
    assert_eq!(entry["type"], "model_change", "{entry}");
    assert_eq!(entry["provider"], "second", "{entry}");
    assert_eq!(entry["modelId"], "model-b", "{entry}");

    // 查不到的模型：pi 抛 `Model not found: p/m` → 500 + error
    let missing = call(
        app_with_two_models(&root, cwd, &agent_dir),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"set_model","provider":"ghost","modelId":"nope"}"#),
        &[],
    )
    .await;
    assert_eq!(missing.status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        missing.json()["error"]
            .as_str()
            .unwrap_or_default()
            .contains("Model not found"),
        "{}",
        missing.text()
    );

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
}

/// 消息上的「从此处编辑」→ `POST /api/agent/{id} {"type":"navigate_tree"}`。
///
/// 这条命令以前没实现，落到 `send` 的兜底分支报「不支持的 agent 命令：
/// navigate_tree」；前端 `handleEditFromHere` 把 500 吞进 `console.error` 后
/// 返回 false —— 按钮点了完全没反应，消息内容也不会回填到编辑器。
///
/// 拿到返回值后前端一定接着 `loadContext(sid, leafId)`（`?leafId=…`），
/// 所以「叶子挪到位 + context 截止到那条消息」才算完整可用。
#[tokio::test]
async fn navigate_tree_moves_leaf_and_truncates_context() {
    let root = fixture_root("navigatetree");
    let agent_dir = fixture_root("navigatetree-agent");
    let cwd = Path::new("/tmp/demo");
    write_session(
        &root,
        &[
            r#"{"type":"message","id":"m0","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"第一个问题"}],"timestamp":1794292905000}}"#,
            r#"{"type":"message","id":"m1","parentId":"m0","timestamp":"2026-11-10T06:41:46.000Z","message":{"role":"assistant","content":[{"type":"text","text":"第一个答案"}],"api":"openai-responses","provider":"qoder","model":"x","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1794292906000}}"#,
            r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-11-10T06:41:47.000Z","message":{"role":"user","content":[{"type":"text","text":"第二个问题"}],"timestamp":1794292907000}}"#,
            r#"{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-11-10T06:41:48.000Z","message":{"role":"assistant","content":[{"type":"text","text":"第二个答案"}],"api":"openai-responses","provider":"qoder","model":"x","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"reasoning":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1794292908000}}"#,
        ],
    );
    let router = app_with_two_models(&root, cwd, &agent_dir);

    // 没有 leafId 时取文件尾部：四条全在
    let full = get(router.clone(), "/api/sessions/sess-a/context")
        .await
        .json();
    assert_eq!(
        full["context"]["messages"].as_array().unwrap().len(),
        4,
        "{full}"
    );

    let reply = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"navigate_tree","targetId":"m2"}"#),
        &[],
    )
    .await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();
    assert_eq!(body["success"], true, "{body}");
    assert_eq!(body["data"]["cancelled"], false, "{body}");
    assert_eq!(body["data"]["leafId"], "m2", "{body}");

    // `loadContext(sid, leafId)`：编辑区要能取回被编辑的那条消息，
    // 上下文得截止到它（含），后面的回答不能再被带进下一轮
    let after = get(
        router.clone(),
        "/api/sessions/sess-a/context?leafId=m2&deferThinking=1&deferMedia=1",
    )
    .await
    .json();
    let messages = after["context"]["messages"].as_array().unwrap();
    assert_eq!(messages.len(), 3, "{after}");
    assert_eq!(messages.last().unwrap()["role"], "user", "{after}");
    assert_eq!(
        messages.last().unwrap()["content"][0]["text"],
        "第二个问题",
        "{after}"
    );
    assert_eq!(after["paging"]["totalMessages"], 3, "{after}");

    // 「从此处编辑」真正要的是：接下来的一条新条目成为 m2 的**子节点**，
    // 即一条新分支，而不是接在 m3 后面。`set_model` 是纯本地写盘的 lane
    // 追加，用它看 parentId 最直接（不用真的发 prompt 打模型）。
    let retarget = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"navigate_tree","targetId":"m2"}"#),
        &[],
    )
    .await;
    assert_eq!(retarget.status, StatusCode::OK, "{}", retarget.text());
    let switched = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"set_model","provider":"second","modelId":"model-b"}"#),
        &[],
    )
    .await;
    assert_eq!(switched.status, StatusCode::OK, "{}", switched.text());
    let text =
        std::fs::read_to_string(root.join("--tmp-demo--/2026-11-10T06-41-44-431Z_sess-a.jsonl"))
            .unwrap();
    let last: Value = serde_json::from_str(text.lines().next_back().unwrap()).unwrap();
    assert_eq!(last["type"], "model_change", "{last}");
    assert_eq!(
        last["parentId"], "m2",
        "新条目得挂在被编辑的消息下，否则就不是分支：{last}"
    );

    // 同一个活会话再挪一次（消息树点节点 / 分支下拉切换叶子）：
    // 返回的 leafId 必须是**导航后**的叶子，不是上一次的
    let back = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"navigate_tree","targetId":"m0"}"#),
        &[],
    )
    .await;
    assert_eq!(back.status, StatusCode::OK, "{}", back.text());
    assert_eq!(back.json()["data"]["leafId"], "m0", "{}", back.text());

    // 不存在的条目：pi 抛 `Entry <id> not found` → 500
    let ghost = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"navigate_tree","targetId":"ghost"}"#),
        &[],
    )
    .await;
    assert_eq!(
        ghost.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        ghost.text()
    );

    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
}

#[tokio::test]
async fn unknown_api_paths_stay_json_404() {
    // 静态兜底不能把 `/api/*` 吞成 HTML —— 前端 `res.json()` 会炸。
    let root = fixture_root("unknownapi");
    let reply = get(app(&root, &root), "/api/does-not-exist").await;
    assert_eq!(reply.status, StatusCode::NOT_FOUND);
    assert_eq!(
        reply
            .header("content-type")
            .and_then(|v| v.split(';').next()),
        Some("application/json"),
        "{}",
        reply.text()
    );
    std::fs::remove_dir_all(&root).ok();
}

// ============================================================================
// 5) 手机访问（会改进程环境，串行执行）
// ============================================================================

/// 手机相关的用例都要改 `TOILER_AGENT_DIR`（进程级），所以集中成一条用例，
/// 避免并行测试互相看到对方的 agent 目录。
#[tokio::test]
// 串行锁跨 await 持有是故意的：这些测试都在各自的 tokio runtime 线程上 block_on，
// 持锁方不会被别人的 lock() 卡住（不会被反过来等待），而 `TOILER_AGENT_DIR` 这类
// 进程级变量必须整段互斥。
#[allow(clippy::await_holding_lock)]
async fn mobile_access_and_pair_share_one_contract() {
    let _env = env_guard();
    let previous = std::env::var_os("TOILER_AGENT_DIR");
    let agent_dir = fixture_root("mobileagent");
    std::env::set_var("TOILER_AGENT_DIR", &agent_dir);
    let sessions = fixture_root("mobilesessions");
    let cwd = fixture_root("mobilecwd");

    // 初始：没有配置文件 → 一切关闭
    let access = get(app(&sessions, &cwd), "/api/mobile/access").await.json();
    assert_eq!(access["enabled"], false);
    assert_eq!(access["password"], Value::Null);
    assert_eq!(access["lanPort"], Value::Null);
    assert_eq!(access["minPasswordLength"], 8);
    assert_eq!(access["public"]["mode"], "quick");

    let pair = get(app(&sessions, &cwd), "/api/mobile/pair").await.json();
    assert_eq!(pair["url"], Value::Null);
    assert_eq!(pair["publicUrl"], Value::Null);
    for field in [
        "host",
        "port",
        "lanEnabled",
        "passwordRequired",
        "desktopShell",
        "reason",
        "publicState",
    ] {
        assert!(pair.get(field).is_some(), "pair 缺字段 {field}：{pair}");
    }

    // 弱密码被拒
    let weak = call(
        app(&sessions, &cwd),
        "PUT",
        "/api/mobile/access",
        Some(r#"{"enabled":true,"password":"123"}"#),
        &[],
    )
    .await;
    assert_eq!(weak.status, StatusCode::BAD_REQUEST);
    assert_eq!(weak.json()["code"], "too-short");

    // 开局域网但没密码被拒
    let no_password = call(
        app(&sessions, &cwd),
        "PUT",
        "/api/mobile/access",
        Some(r#"{"enabled":true}"#),
        &[],
    )
    .await;
    assert_eq!(no_password.status, StatusCode::BAD_REQUEST);
    assert_eq!(no_password.json()["code"], "password-required");

    // 正常保存
    let saved = call(
        app(&sessions, &cwd),
        "PUT",
        "/api/mobile/access",
        Some(r#"{"enabled":true,"password":"12345678"}"#),
        &[],
    )
    .await;
    assert_eq!(saved.status, StatusCode::OK, "{}", saved.text());
    let saved_body = saved.json();
    assert_eq!(saved_body["saved"], true);
    assert_eq!(saved_body["password"], "12345678");

    // 落盘文件必须是 0600（里面有密码）
    let configs = agent_dir.join("configs");
    let config = configs.join("desktop-access.json");
    assert!(
        config.exists(),
        "配置应写到 <toilerHome>/configs/，实际找的是 {}",
        config.display()
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&config).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "配置里有密码，必须只给属主读");
    }

    // 代理回写端口后，配对地址会用它（而不是回环端口）
    let runtime = configs.join("desktop-access.runtime.json");
    let own_pid = std::process::id();
    std::fs::write(
        &runtime,
        format!(r#"{{"lanPort":30142,"ownerPid":{own_pid}}}"#),
    )
    .unwrap();
    std::env::set_var("PI_WEB_SHELL_PID", own_pid.to_string());
    let paired = get(
        app(&sessions, &cwd),
        &format!(
            "/api/mobile/pair?session={}",
            "01a1c511-ef4b-7516-b8ed-33bd756700bc"
        ),
    )
    .await
    .json();
    let url = paired["url"].as_str().unwrap_or_default();
    assert!(url.contains(":30142/m?session="), "{paired}");
    assert_eq!(paired["reason"], "ok");
    std::env::remove_var("PI_WEB_SHELL_PID");

    // named 模式 + 非法域名被拒
    let bad_host = call(
        app(&sessions, &cwd),
        "PUT",
        "/api/mobile/access",
        Some(r#"{"enabled":true,"public":{"enabled":true,"mode":"named","hostname":"not a domain"}}"#),
        &[],
    )
    .await;
    assert_eq!(bad_host.status, StatusCode::BAD_REQUEST);
    assert_eq!(bad_host.json()["code"], "invalid-hostname");

    // named 模式 + Unicode 域名：落盘成 punycode，展示回 Unicode
    let named = call(
        app(&sessions, &cwd),
        "PUT",
        "/api/mobile/access",
        Some(r#"{"enabled":true,"public":{"enabled":true,"mode":"named","hostname":"π.works"}}"#),
        &[],
    )
    .await;
    assert_eq!(named.status, StatusCode::OK, "{}", named.text());
    let named_body = named.json();
    assert_eq!(named_body["public"]["hostname"], "xn--1xa.works");
    assert_eq!(named_body["public"]["mode"], "named");
    assert_eq!(named_body["publicHostnameDisplay"], "π.works");

    // 隧道跑起来后 publicUrl 用 Unicode 域名
    std::fs::write(
        configs.join("desktop-access.tunnel.json"),
        r#"{"state":"running","mode":"named","url":null,"hostname":"xn--1xa.works","pid":1,
            "restarts":0,"lastError":null,"logTail":[],"updatedAtMs":1}"#,
    )
    .unwrap();
    let with_public = get(app(&sessions, &cwd), "/api/mobile/pair").await.json();
    assert_eq!(with_public["publicState"], "running");
    assert_eq!(
        with_public["publicUrl"], "https://π.works/m",
        "{with_public}"
    );

    // 恢复环境，别影响别的测试
    match previous {
        Some(value) => std::env::set_var("TOILER_AGENT_DIR", value),
        None => std::env::remove_var("TOILER_AGENT_DIR"),
    }
    std::fs::remove_dir_all(&agent_dir).ok();
    std::fs::remove_dir_all(&sessions).ok();
    std::fs::remove_dir_all(&cwd).ok();
}

// ============================================================================
// 6) 历史会话全量回归
// ============================================================================

/// 把本机 `~/.pi/agent/sessions` 里**每一个**会话都过一遍 detail，
/// 确认「全部可见可续聊」这条验收标准不是抽样结论。
///
/// 560 个真实会话逐条串行请求太慢（每个都要重扫全部会话才能建立 parentSessionId
/// 索引），所以这里直接用会话层的 `SessionList`：`/api/sessions/{id}` 的实现就是
/// 它 + `session_view`，绕过 HTTP 不损失覆盖。
///
/// 没有真实会话（CI）时自动跳过：断言的是「扫描能跑完」，不是「一定有 N 个」。
///
/// 标记 `#[ignore]`：560 个真实会话要跑 80 秒左右，不适合放进每次 `cargo test`。
/// 手动跑：`cargo test -p toiler-server --test regression -- --ignored --nocapture`
#[tokio::test]
#[ignore = "需要真实的 ~/.pi/agent/sessions，且全量扫描约 80s"]
async fn every_real_pi_session_loads() {
    let _ = tracing_subscriber::fmt().try_init();
    let Ok(home) = std::env::var("HOME") else {
        return;
    };
    let root = PathBuf::from(home).join(".pi/agent/sessions");
    if !root.is_dir() {
        eprintln!("跳过：{} 不存在", root.display());
        return;
    }

    let catalog = ModelCatalog::from_config(json!({ "providers": {} }), None).unwrap();
    let state = AppState::with_cwd(&root, catalog, &root);
    let listed = state.sessions().list(false);
    if listed.is_empty() {
        eprintln!("跳过：没有会话文件");
        return;
    }

    let mut failures: Vec<String> = Vec::new();
    let mut entry_total = 0usize;
    let mut message_total = 0usize;

    for item in &listed {
        let Some(id) = item["id"].as_str() else {
            failures.push(format!("列表项缺 id：{item}"));
            continue;
        };
        let Some(session) = state.sessions().session(id) else {
            failures.push(format!("{id}：读不出会话"));
            continue;
        };
        entry_total += session.entries.len();
        message_total += session.message_count();

        // 详情页的三块结构：tree / context / paging（与 GET /api/sessions/{id}
        // 走同一批 session_view 函数，所以这里等价于打一遍那条接口）。
        let labels = session.labels();
        let tree = toiler_server::session_view::tree_projection(
            &toiler_server::session_view::build_tree(&session.entries, &labels),
        );
        if tree.as_array().map(Vec::is_empty).unwrap_or(true) {
            failures.push(format!("{id} 没有 tree"));
        }

        let view = toiler_server::session_view::context_view(&session.entries, None);
        if session.message_count() > 0 && view.messages.is_empty() {
            failures.push(format!(
                "{id} 有 {} 条消息但 context 为空",
                session.message_count()
            ));
        }

        // 前端读 `toolName` / `toolCallId` / `input`。不归一化就整页白屏，
        // 所以这一条必须对每个会话都成立（不是抽样）。
        for message in &view.messages {
            if message.get("role").and_then(serde_json::Value::as_str) == Some("system") {
                failures.push(format!("{id} 的 context 里出现了 system 消息"));
                break;
            }
            if message.get("role").and_then(serde_json::Value::as_str) != Some("assistant") {
                continue;
            }
            let Some(blocks) = message.get("content").and_then(serde_json::Value::as_array) else {
                continue;
            };
            let broken = blocks.iter().any(|block| {
                block.get("type").and_then(serde_json::Value::as_str) == Some("toolCall")
                    && (block
                        .get("toolName")
                        .and_then(serde_json::Value::as_str)
                        .is_none()
                        || block
                            .get("toolCallId")
                            .and_then(serde_json::Value::as_str)
                            .is_none()
                        || block.get("input").map(|v| v.is_object()) != Some(true))
            });
            if broken {
                failures.push(format!("{id} 的 toolCall 块没有归一化成 toolName/input"));
                break;
            }
        }
        if view.messages.len() > view.entry_ids.len() {
            failures.push(format!("{id} 的 messages 比 entryIds 多，分页会错位"));
        }

        let leaf = toiler_server::session_view::leaf_id(&session.entries);
        if leaf.is_none() && session.message_count() > 0 {
            failures.push(format!("{id} 有消息但没有 leafId（续聊会没有锚点）"));
        }

        // 分页：默认 120 条，起点不能越界
        let page = toiler_server::session_view::page_tail(&view.messages, None);
        if page.end > view.messages.len() || page.start > page.end {
            failures.push(format!(
                "{id} 分页越界：start={} end={} len={}",
                page.start,
                page.end,
                view.messages.len()
            ));
        }

        // 元数据：侧边栏要 `info`，缺了那一行就渲染不出来
        if item["name"].is_null()
            && item["firstMessage"].as_str().unwrap_or("").is_empty()
            && session.message_count() > 0
        {
            failures.push(format!("{id} 既没有名字也没有首条消息"));
        }
    }

    assert!(
        failures.is_empty(),
        "{} / {} 个会话读取失败：\n{}",
        failures.len(),
        listed.len(),
        failures
            .iter()
            .take(20)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
    eprintln!(
        "回归通过：{} 个会话、{} 条条目、{} 条消息",
        listed.len(),
        entry_total,
        message_total
    );
}

/// 项目根/分支探测：真实 git 仓库才能验证，所以只在当前仓库上跑。
#[tokio::test]
async fn real_project_root_is_detected_for_a_git_checkout() {
    let Ok(manifest) = std::env::var("CARGO_MANIFEST_DIR") else {
        return;
    };
    // CARGO_MANIFEST_DIR 指向 crates/toiler-server，仓库根在两级之上
    let repo = Path::new(&manifest)
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf);
    let Some(repo) = repo.filter(|path| path.join(".git").exists()) else {
        eprintln!("跳过：{manifest} 之上没有 .git");
        return;
    };

    let body = get(app(&repo, &repo), "/api/sessions").await.json();
    assert!(body["sessions"].is_array());

    // git/status 也要能读（前端文件面板用它画状态标记）
    let url = format!(
        "/api/git/status?cwd={}",
        url_encode(&repo.to_string_lossy())
    );
    let status = get(app(&repo, &repo), &url).await.json();
    assert_eq!(status["isGitRepository"], true, "{status}");
    let _: BTreeMap<String, Value> = BTreeMap::new();
}

// ============================================================================
// 6) /api/skills —— 技能面板
// ============================================================================

#[tokio::test]
async fn skills_lists_project_skill_with_source_info() {
    // 前端 SkillsConfig 依赖：`GET /api/skills?cwd=` 的
    // `skills[].{name,description,filePath,baseDir,disableModelInvocation,sourceInfo}`、
    // `diagnostics[]`、`projectResourcesLoaded`。
    // 少任何一个字段，面板要么整块不渲染，要么所有技能都落到 Other 组。
    let root = fixture_root("skills");
    let store = fixture_root("skills-store");
    let skill_dir = root.join(".agents/skills/demo-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: demo-skill\ndescription: 测试用技能\ndisable-model-invocation: true\n---\n\n正文\n",
    )
    .unwrap();
    // 项目技能只在项目被信任时加载（见 routes/skills.rs 头部），先落一条信任决定。
    toiler_server::trust::TrustStore::at(store.join("trust.json"))
        .set(root.to_str().unwrap(), Some(true))
        .unwrap();

    let url = format!("/api/skills?cwd={}", url_encode(&root.to_string_lossy()));
    let reply = get(app_with_agent_dir(&store, &root, Some(&store)), &url).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());

    let body = reply.json();
    let skills = body["skills"].as_array().unwrap();
    let demo = skills
        .iter()
        .find(|s| s["name"] == "demo-skill")
        .unwrap_or_else(|| panic!("项目技能应出现在列表里：{body}"));

    assert_eq!(demo["description"], "测试用技能");
    assert_eq!(
        demo["disableModelInvocation"], true,
        "frontmatter 的开关要透传"
    );
    assert_eq!(
        demo["sourceInfo"]["source"], "project",
        "前端按 source 分 Project 组"
    );
    assert_eq!(demo["sourceInfo"]["scope"], "project");
    assert!(
        demo["filePath"]
            .as_str()
            .unwrap()
            .ends_with(".agents/skills/demo-skill/SKILL.md"),
        "{demo}"
    );
    assert!(
        demo["baseDir"].as_str().unwrap().ends_with("demo-skill"),
        "{demo}"
    );

    // `projectResourcesLoaded` 为 false 时前端禁用「Project」作用域按钮，所以只要布尔值。
    assert!(body["projectResourcesLoaded"].as_bool().is_some(), "{body}");
    assert!(body["diagnostics"].is_array(), "{body}");

    // 缺 cwd：pi 返回 400，而不是 200 空列表（避免前端把「参数丢了」当成「没技能」）。
    let missing = get(
        app_with_agent_dir(&store, &root, Some(&store)),
        "/api/skills",
    )
    .await;
    assert_eq!(
        missing.status,
        StatusCode::BAD_REQUEST,
        "{}",
        missing.text()
    );

    // 技能市场与开关未实现 → 501 + 中文说明，前端会把 error 原文显示出来。
    let search = call(
        app_with_agent_dir(&store, &root, Some(&store)),
        "POST",
        "/api/skills/search",
        Some("{\"query\":\"x\"}"),
        &[],
    )
    .await;
    assert_eq!(search.status, StatusCode::NOT_IMPLEMENTED);
    assert!(search.json()["error"]
        .as_str()
        .unwrap()
        .contains("技能市场"));

    let toggle = call(
        app_with_agent_dir(&store, &root, Some(&store)),
        "PATCH",
        "/api/skills",
        Some("{\"filePath\":\"/x/SKILL.md\",\"disableModelInvocation\":true}"),
        &[],
    )
    .await;
    assert_eq!(toggle.status, StatusCode::NOT_IMPLEMENTED);
}

#[tokio::test]
async fn skills_climb_to_git_root_and_stop_there() {
    // pi 的 `collectAncestorAgentsSkillDirs`：从 cwd 逐层向上找 `.agents/skills`，
    // 碰到 git 仓库根就停。所以「同一个仓库里共享」生效，「仓库外的父目录」不生效。
    let outer = fixture_root("skills-outer");
    let repo = outer.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    for (dir, name) in [
        (outer.join(".agents/skills"), "outside-skill"),
        (repo.join(".agents/skills"), "inside-skill"),
    ] {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: 测试用技能\n---\n"),
        )
        .unwrap();
    }

    let store = fixture_root("skills-git-store");
    toiler_server::trust::TrustStore::at(store.join("trust.json"))
        .set(repo.to_str().unwrap(), Some(true))
        .unwrap();

    let url = format!("/api/skills?cwd={}", url_encode(&repo.to_string_lossy()));
    let body = get(app_with_agent_dir(&store, &repo, Some(&store)), &url)
        .await
        .json();
    let names: Vec<&str> = body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|skill| skill["name"].as_str())
        .collect();

    assert!(
        names.contains(&"inside-skill"),
        "git 根之内的项目技能要出现：{body}"
    );
    assert!(
        !names.contains(&"outside-skill"),
        "git 根之外的父目录技能不该出现：{body}"
    );
}

#[tokio::test]
async fn skills_hidden_when_project_untrusted() {
    // 未信任的项目：技能不列出，且 `projectResourcesLoaded=false`
    // ——前端据此禁用「Project」作用域按钮，语义必须与列表一致。
    let root = fixture_root("skills-untrusted");
    let skill_dir = root.join(".agents/skills/secret-skill");
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        "---\nname: secret-skill\ndescription: 未信任项目\n---\n",
    )
    .unwrap();

    let store = fixture_root("skills-untrusted-store");
    let url = format!("/api/skills?cwd={}", url_encode(&root.to_string_lossy()));
    let body = get(app_with_agent_dir(&store, &root, Some(&store)), &url)
        .await
        .json();

    assert_eq!(body["projectResourcesLoaded"], false, "{body}");
    let names: Vec<&str> = body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|skill| skill["name"].as_str())
        .collect();
    assert!(
        !names.contains(&"secret-skill"),
        "未信任的项目技能不加载：{body}"
    );
}

#[tokio::test]
async fn skills_come_from_project_root_not_cwd() {
    // 约定：项目技能放在 `$project/.agents/skills`。
    // 子目录里开会话时，仍然读**项目根**那一份；子目录自己的 `.agents/skills`
    // 不算（那是 cwd 级，不在约定里）。
    let outer = fixture_root("skills-root");
    let repo = outer.join("repo");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let sub = repo.join("frontend/deep");
    std::fs::create_dir_all(&sub).unwrap();

    for (dir, name) in [
        (repo.join(".agents/skills"), "root-skill"),
        (sub.join(".agents/skills"), "subdir-skill"),
    ] {
        let skill_dir = dir.join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: 测试用技能\n---\n"),
        )
        .unwrap();
    }

    let store = fixture_root("skills-root-store");
    toiler_server::trust::TrustStore::at(store.join("trust.json"))
        .set(repo.to_str().unwrap(), Some(true))
        .unwrap();

    let url = format!("/api/skills?cwd={}", url_encode(&sub.to_string_lossy()));
    let body = get(app_with_agent_dir(&store, &sub, Some(&store)), &url)
        .await
        .json();
    let names: Vec<&str> = body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|skill| skill["name"].as_str())
        .collect();

    assert!(
        names.contains(&"root-skill"),
        "cwd 在子目录时也要读项目根的技能：{body}"
    );
    assert!(
        !names.contains(&"subdir-skill"),
        "子目录自己的 .agents/skills 不属于项目约定：{body}"
    );

    let project = body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .find(|skill| skill["name"] == "root-skill")
        .unwrap();
    assert_eq!(project["sourceInfo"]["scope"], "project");
}

#[tokio::test]
async fn project_and_user_skills_are_merged_even_with_same_name() {
    // 需求：`$project/.agents/skills` 与 `$user/.agents/skills` **合并**返回。
    // 同名技能不再互相顶掉（pi 的 loader 是按 name 去重的，这里刻意不同）。
    let Some(user_skills) =
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".agents/skills"))
    else {
        eprintln!("跳过：没有 HOME");
        return;
    };
    let Some(existing) = std::fs::read_dir(&user_skills).ok().and_then(|entries| {
        entries
            .flatten()
            .find(|entry| entry.path().join("SKILL.md").is_file())
            .map(|entry| entry.file_name().to_string_lossy().to_string())
    }) else {
        eprintln!("跳过：~/.agents/skills 里没有技能");
        return;
    };

    // 项目里放一个**同名**技能，内容不同
    let root = fixture_root("skills-merge");
    let skill_dir = root.join(".agents/skills").join(&existing);
    std::fs::create_dir_all(&skill_dir).unwrap();
    std::fs::write(
        skill_dir.join("SKILL.md"),
        format!("---\nname: {existing}\ndescription: 项目版同名技能\n---\n"),
    )
    .unwrap();

    let store = fixture_root("skills-merge-store");
    toiler_server::trust::TrustStore::at(store.join("trust.json"))
        .set(root.to_str().unwrap(), Some(true))
        .unwrap();

    let url = format!("/api/skills?cwd={}", url_encode(&root.to_string_lossy()));
    let body = get(app_with_agent_dir(&store, &root, Some(&store)), &url)
        .await
        .json();
    let matched: Vec<&Value> = body["skills"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|skill| skill["name"] == existing.as_str())
        .collect();

    assert_eq!(
        matched.len(),
        2,
        "同名技能应同时出现在列表里（project + user）：{body}"
    );
    let sources: Vec<&str> = matched
        .iter()
        .filter_map(|skill| skill["sourceInfo"]["source"].as_str())
        .collect();
    assert!(sources.contains(&"project"), "{body}");
    assert!(sources.contains(&"user"), "{body}");
    assert!(
        matched[0]["sourceInfo"]["source"] == "project",
        "项目版排在前面（先扫到的优先）：{body}"
    );
}

// ============================================================================
// 7) /api/plugins —— 插件面板
// ============================================================================

/// 打一次 `POST /api/plugins`（body 是 JSON）。
async fn post_plugins(root: &Path, configs: &Path, body: Value) -> Reply {
    call(
        app_with_agent_dir(root, root, Some(configs)),
        "POST",
        "/api/plugins",
        Some(&body.to_string()),
        &[],
    )
    .await
}

/// 假的 npm：只做 toiler 真正会调用的两件事（install / uninstall），不碰网络。
/// 真 npm 在离线机器 / CI 上不可用，但这里要验证的是「命令怎么拼、settings 怎么落盘」。
#[cfg(unix)]
const FAKE_NPM: &str = r#"#!/usr/bin/env python3
import json, pathlib, re, shutil, sys

args = sys.argv[1:]
cmd = args[0] if args else ""
prefix = ""
specs = []
i = 1
while i < len(args):
    arg = args[i]
    if arg == "--prefix":
        prefix = args[i + 1]
        i += 2
        continue
    if arg.startswith("-"):
        i += 1
        continue
    specs.append(arg)
    i += 1

for spec in specs:
    name = re.match(r"^(@[^/]+/[^@]+|[^@]+)", spec).group(1)
    target = pathlib.Path(prefix) / "node_modules" / name
    if cmd == "install":
        (target / "extensions").mkdir(parents=True, exist_ok=True)
        (target / "package.json").write_text(json.dumps({"name": name, "version": "9.9.9"}))
        (target / "extensions" / "entry.js").write_text("export default {};")
    elif cmd == "uninstall":
        shutil.rmtree(target, ignore_errors=True)
print("fake npm ok")
"#;

#[tokio::test]
async fn plugins_list_reports_configured_packages_and_install_state() {
    // 前端 PluginsConfig 依赖：`GET /api/plugins?cwd=` 的
    // `packages[].{source,scope,filtered,disabled,installedPath,packageName,version,
    // configuredVersion,counts,resources,status}`、`totals`、`diagnostics`、
    // `projectResourcesLoaded`。
    // 这条路由最初根本不存在：请求落到静态兜底拿到 404 `{"error":"Not found"}`，
    // 前端把这个 error 当成一条插件画在左侧列表里（就是「不显示已安装插件」）。
    let root = fixture_root("plugins");
    let agent = fixture_root("plugins-agent");
    // 生产的配置目录是 `<toilerHome>/configs`（见 paths.rs 的目录布局），
    // 路由按这个位置反推「插件安装根」= `<toilerHome>/plugins`。
    let configs = agent.join("configs");
    std::fs::create_dir_all(&configs).unwrap();

    // 带 `pi` 清单的包：清单声明了一个扩展入口。
    // 注意清单存在时**只**按清单字段收资源（与 pi 的 collectPackageResources 一致），
    // 所以这里故意再放一个技能目录，用来断言它**不**被算进去。
    let manifest_pkg = agent.join("plugins/npm/node_modules/@demo/plugin");
    std::fs::create_dir_all(manifest_pkg.join("dist")).unwrap();
    std::fs::create_dir_all(manifest_pkg.join("skills/ignored")).unwrap();
    std::fs::write(manifest_pkg.join("dist/extension.js"), "export default {};").unwrap();
    std::fs::write(
        manifest_pkg.join("skills/ignored/SKILL.md"),
        "---\nname: ignored\n---\n",
    )
    .unwrap();
    std::fs::write(
        manifest_pkg.join("package.json"),
        json!({
            "name": "@demo/plugin",
            "version": "1.2.3",
            "pi": { "extensions": ["./dist/extension.js"] }
        })
        .to_string(),
    )
    .unwrap();

    // 没有 `pi` 清单的包：按约定目录（skills/、prompts/…）收资源。
    let plain_pkg = agent.join("plugins/npm/node_modules/plain");
    std::fs::create_dir_all(plain_pkg.join("skills/demo")).unwrap();
    std::fs::create_dir_all(plain_pkg.join("prompts")).unwrap();
    std::fs::write(
        plain_pkg.join("skills/demo/SKILL.md"),
        "---\nname: demo\n---\n",
    )
    .unwrap();
    std::fs::write(plain_pkg.join("prompts/hello.md"), "hi").unwrap();
    std::fs::write(
        plain_pkg.join("package.json"),
        json!({ "name": "plain", "version": "0.1.0" }).to_string(),
    )
    .unwrap();

    std::fs::write(
        configs.join("settings.json"),
        json!({
            "packages": [
                "npm:@demo/plugin",
                "npm:plain",
                // 对象形态 + 四个空数组 = 用户禁用了它（pi 的 isDisabledPackage）
                { "source": "npm:off", "extensions": [], "skills": [], "prompts": [], "themes": [] },
                // 配置了但没装：要给出 warning，configuredVersion 取 `@` 之后的版本段
                "npm:absent@^2.0.0"
            ]
        })
        .to_string(),
    )
    .unwrap();
    // 未信任的项目里配一个包：pi 与 toiler 都**不**读项目 settings。
    std::fs::create_dir_all(root.join(".pi")).unwrap();
    std::fs::write(
        root.join(".pi/settings.json"),
        json!({ "packages": ["npm:project-only"] }).to_string(),
    )
    .unwrap();

    let url = format!("/api/plugins?cwd={}", url_encode(&root.to_string_lossy()));
    let reply = get(app_with_agent_dir(&root, &root, Some(&configs)), &url).await;
    assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
    let body = reply.json();

    let packages = body["packages"].as_array().unwrap();
    assert_eq!(packages.len(), 4, "项目未信任时不列项目包：{body}");
    assert_eq!(
        body["projectResourcesLoaded"], false,
        "未信任时前端要禁用 Project 作用域"
    );

    let demo = packages
        .iter()
        .find(|pkg| pkg["source"] == "npm:@demo/plugin")
        .unwrap_or_else(|| panic!("已安装的包要出现在列表里：{body}"));
    assert_eq!(demo["scope"], "global", "pi 内部叫 user，对外是 global");
    assert_eq!(demo["filtered"], false, "字符串条目不是 filtered");
    assert_eq!(demo["disabled"], false);
    assert_eq!(demo["packageName"], "@demo/plugin");
    assert_eq!(demo["version"], "1.2.3");
    assert_eq!(demo["status"], "loaded", "有资源就是 loaded：{demo}");
    assert_eq!(demo["counts"]["extensions"], 1);
    assert_eq!(demo["counts"]["skills"], 0, "有清单时不回退约定目录");
    let resource = &demo["resources"][0];
    assert_eq!(resource["kind"], "extension");
    assert_eq!(
        resource["name"], "extension",
        "去掉扩展名（index.ts 才取目录名）"
    );
    assert_eq!(resource["relativePath"], "dist/extension.js");
    assert!(
        demo["installedPath"]
            .as_str()
            .unwrap()
            .ends_with("plugins/npm/node_modules/@demo/plugin"),
        "{demo}"
    );

    let plain = packages
        .iter()
        .find(|pkg| pkg["source"] == "npm:plain")
        .unwrap_or_else(|| panic!("约定目录形态的包也要列出来：{body}"));
    assert_eq!(plain["counts"]["skills"], 1);
    assert_eq!(plain["counts"]["prompts"], 1);
    assert_eq!(plain["resources"][0]["kind"], "skill");
    assert_eq!(plain["resources"][0]["name"], "demo", "SKILL.md 取目录名");

    let off = packages
        .iter()
        .find(|pkg| pkg["source"] == "npm:off")
        .unwrap();
    assert_eq!(off["filtered"], true);
    assert_eq!(off["disabled"], true);
    assert_eq!(off["status"], "disabled", "禁用优先于其它状态：{off}");

    let absent = packages
        .iter()
        .find(|pkg| pkg["source"] == "npm:absent@^2.0.0")
        .unwrap();
    assert_eq!(absent["status"], "missing");
    assert_eq!(absent["configuredVersion"], "^2.0.0");
    assert!(
        absent.get("installedPath").is_none(),
        "没安装就不该有 installedPath 键（pi 的 JSON.stringify 会丢掉 undefined）：{absent}"
    );

    // 每个未安装的包两条 warning（pi 的 resolve 与列表各一条），文案逐字一致。
    let diagnostics = body["diagnostics"].as_array().unwrap();
    let messages: Vec<&str> = diagnostics
        .iter()
        .filter_map(|item| item["message"].as_str())
        .collect();
    assert_eq!(
        messages
            .iter()
            .filter(|m| **m == "Package is configured but not installed yet.")
            .count(),
        2,
        "{body}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| **m == "Configured package path was not found.")
            .count(),
        2,
        "{body}"
    );

    assert_eq!(
        body["totals"]["extensions"], 1,
        "totals 只统计包内资源：{body}"
    );
    assert_eq!(body["totals"]["skills"], 1);
    assert_eq!(body["totals"]["prompts"], 1);
    assert_eq!(body["totals"]["themes"], 0);

    // 信任之后项目包才出现，且作用域是 project。
    toiler_server::trust::TrustStore::at(configs.join("trust.json"))
        .set(root.to_str().unwrap(), Some(true))
        .unwrap();
    let trusted = get(app_with_agent_dir(&root, &root, Some(&configs)), &url)
        .await
        .json();
    assert_eq!(trusted["projectResourcesLoaded"], true, "{trusted}");
    assert!(
        trusted["packages"]
            .as_array()
            .unwrap()
            .iter()
            .any(|pkg| pkg["source"] == "npm:project-only" && pkg["scope"] == "project"),
        "信任后项目包要出现：{trusted}"
    );

    // 缺 cwd：与 pi 一致返回 400（否则前端会把「参数丢了」当成「没有插件」）。
    let missing = get(
        app_with_agent_dir(&root, &root, Some(&configs)),
        "/api/plugins",
    )
    .await;
    assert_eq!(
        missing.status,
        StatusCode::BAD_REQUEST,
        "{}",
        missing.text()
    );

    // POST 的校验分支（真正装包的那条见下一个用例）：前端会把 error 原文画在面板上，
    // 所以状态码和文案都要能直接看懂。
    let no_cwd = post_plugins(
        &root,
        &configs,
        json!({"action": "install", "source": "npm:x"}),
    )
    .await;
    assert_eq!(no_cwd.status, StatusCode::BAD_REQUEST, "{}", no_cwd.text());
    assert_eq!(no_cwd.json()["error"], "cwd required");

    let no_action = post_plugins(&root, &configs, json!({"cwd": root.to_str().unwrap()})).await;
    assert_eq!(
        no_action.status,
        StatusCode::BAD_REQUEST,
        "{}",
        no_action.text()
    );
    assert_eq!(no_action.json()["error"], "action required");

    let unknown = post_plugins(
        &root,
        &configs,
        json!({"cwd": root.to_str().unwrap(), "action": "explode", "source": "npm:x"}),
    )
    .await;
    assert_eq!(
        unknown.status,
        StatusCode::BAD_REQUEST,
        "{}",
        unknown.text()
    );
    assert!(unknown.json()["error"]
        .as_str()
        .unwrap()
        .contains("Unsupported action: explode"));

    let no_source = post_plugins(
        &root,
        &configs,
        json!({"cwd": root.to_str().unwrap(), "action": "install"}),
    )
    .await;
    assert_eq!(
        no_source.status,
        StatusCode::BAD_REQUEST,
        "{}",
        no_source.text()
    );
    assert_eq!(no_source.json()["error"], "source required");

    // 插件市场返回空页：抽屉显示「没有结果」，而不是一条 404 错误。
    let catalog = get(
        app_with_agent_dir(&root, &root, Some(&configs)),
        "/api/plugins/catalog?q=pi&from=0&size=40",
    )
    .await;
    assert_eq!(catalog.status, StatusCode::OK, "{}", catalog.text());
    let catalog_body = catalog.json();
    assert_eq!(catalog_body["plugins"].as_array().unwrap().len(), 0);
    assert_eq!(catalog_body["hasMore"], false);
}

#[cfg(unix)]
#[tokio::test]
// 串行锁跨 await 持有是故意的：这些测试都在各自的 tokio runtime 线程上 block_on，
// 持锁方不会被别人的 lock() 卡住（不会被反过来等待），而 `TOILER_AGENT_DIR` 这类
// 进程级变量必须整段互斥。
#[allow(clippy::await_holding_lock)]
async fn plugins_install_remove_run_local_npm_and_persist_settings() {
    // 前端 PluginsConfig 的三个按钮直接依赖它：
    // `install`（抽屉「添加插件」）、`remove`（详情页「移除」）、`disable`/`enable`（详情页开关）。
    // 装包必须走本机 npm；找不到 npm 时要给一句能看懂的提示，而不是 `spawn npm ENOENT`。
    use std::os::unix::fs::PermissionsExt;

    let _env = env_guard();
    let root = fixture_root("plugins-npm");
    let agent = fixture_root("plugins-npm-agent");
    let configs = agent.join("configs");
    std::fs::create_dir_all(&configs).unwrap();
    std::fs::write(
        configs.join("settings.json"),
        json!({ "packages": [], "defaultProvider": "keep-me" }).to_string(),
    )
    .unwrap();

    let bin = agent.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let npm = bin.join("npm");
    std::fs::write(&npm, FAKE_NPM).unwrap();
    std::fs::set_permissions(&npm, std::fs::Permissions::from_mode(0o755)).unwrap();
    std::env::set_var("TOILER_NPM", &npm);

    // 项目作用域要先信任项目（这里 trust.json 还没写）：否则一个刚 clone 的仓库
    // 就能靠 `.pi/settings.json` 决定装什么。注意「需要信任」的前提是项目里已经有
    // 需要信任的资源（`trust.rs` 的 TRUST_REQUIRING），所以先落一个 `.pi/settings.json`。
    let project_settings = root.join(".pi").join("settings.json");
    std::fs::create_dir_all(project_settings.parent().unwrap()).unwrap();
    std::fs::write(&project_settings, json!({ "packages": [] }).to_string()).unwrap();
    let untrusted = post_plugins(
        &root,
        &configs,
        json!({
            "cwd": root.to_str().unwrap(),
            "action": "install",
            "source": "npm:x",
            "scope": "project",
        }),
    )
    .await;
    assert_eq!(
        untrusted.status,
        StatusCode::FORBIDDEN,
        "{}",
        untrusted.text()
    );
    assert!(
        untrusted.json()["error"]
            .as_str()
            .unwrap()
            .contains("must be trusted"),
        "{}",
        untrusted.text()
    );
    assert_eq!(
        read_settings_file(&project_settings)["packages"],
        json!([]),
        "被拒绝的安装不能改动项目 settings"
    );

    let read_settings = || -> Value { read_settings_file(&configs.join("settings.json")) };

    // ---- install：npm 装到 <toilerHome>/plugins/npm，并把条目写进 settings.json
    let installed = post_plugins(
        &root,
        &configs,
        json!({
            "cwd": root.to_str().unwrap(),
            "action": "install",
            "source": "npm:pi-mcp-adapter@1.0.0",
            "scope": "global",
        }),
    )
    .await;
    assert_eq!(installed.status, StatusCode::OK, "{}", installed.text());
    let body = installed.json();
    let package = body["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pkg| pkg["source"] == "npm:pi-mcp-adapter@1.0.0")
        .unwrap_or_else(|| panic!("装完要出现在列表里：{body}"));
    assert_eq!(
        package["status"], "loaded",
        "假 npm 也写了 extensions/entry.js"
    );
    assert_eq!(package["version"], "9.9.9", "版本来自已装包的 package.json");
    assert!(
        package["installedPath"]
            .as_str()
            .unwrap()
            .ends_with("plugins/npm/node_modules/pi-mcp-adapter"),
        "全局安装根是 <toilerHome>/plugins/npm：{package}"
    );
    assert!(
        agent
            .join("plugins/npm/node_modules/pi-mcp-adapter/package.json")
            .is_file(),
        "包要真的落到磁盘上"
    );
    let settings = read_settings();
    assert_eq!(settings["packages"][0], "npm:pi-mcp-adapter@1.0.0");
    assert_eq!(
        settings["defaultProvider"], "keep-me",
        "写 packages 不能丢掉 settings.json 的其它键"
    );
    assert!(
        configs.join("settings.json").is_file(),
        "settings.json 路径不能变"
    );

    // ---- install 是幂等的：同一个包再装一次不会出现两条
    let again = post_plugins(
        &root,
        &configs,
        json!({
            "cwd": root.to_str().unwrap(),
            "action": "install",
            "source": "npm:pi-mcp-adapter@1.0.0",
            "scope": "global",
        }),
    )
    .await;
    assert_eq!(again.status, StatusCode::OK, "{}", again.text());
    assert_eq!(read_settings()["packages"].as_array().unwrap().len(), 1);

    // ---- disable / enable：只改 settings.json 的条目形态，不碰磁盘
    let source = "npm:pi-mcp-adapter@1.0.0";
    let disabled = post_plugins(
        &root,
        &configs,
        json!({"cwd": root.to_str().unwrap(), "action": "disable", "source": source}),
    )
    .await;
    assert_eq!(disabled.status, StatusCode::OK, "{}", disabled.text());
    let disabled_body = disabled.json();
    let disabled_pkg = disabled_body["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|pkg| pkg["source"] == source)
        .unwrap();
    assert_eq!(disabled_pkg["disabled"], true);
    assert_eq!(disabled_pkg["status"], "disabled");
    assert_eq!(settings["packages"].as_array().unwrap().len(), 1);
    let settings_after_disable = read_settings();
    assert_eq!(settings_after_disable["packages"][0]["source"], source);
    assert_eq!(
        settings_after_disable["packages"][0]["extensions"],
        json!([])
    );

    let enabled = post_plugins(
        &root,
        &configs,
        json!({"cwd": root.to_str().unwrap(), "action": "enable", "source": source}),
    )
    .await;
    assert_eq!(enabled.status, StatusCode::OK, "{}", enabled.text());
    assert_eq!(
        enabled.json()["packages"][0]["disabled"],
        false,
        "{}",
        enabled.text()
    );
    assert_eq!(
        read_settings()["packages"][0],
        source,
        "启用后回到字符串形态"
    );

    // ---- remove：npm uninstall + 从 settings 删除条目
    let removed = post_plugins(
        &root,
        &configs,
        json!({"cwd": root.to_str().unwrap(), "action": "remove", "source": source}),
    )
    .await;
    assert_eq!(removed.status, StatusCode::OK, "{}", removed.text());
    assert!(
        removed.json()["packages"].as_array().unwrap().is_empty(),
        "{}",
        removed.text()
    );
    assert_eq!(read_settings()["packages"], json!([]));
    assert!(
        !agent
            .join("plugins/npm/node_modules/pi-mcp-adapter")
            .exists(),
        "卸载要真的删掉目录"
    );

    // ---- 找不到 npm：给一句能看懂的中文提示（不是 spawn ENOENT）
    std::env::set_var("TOILER_NPM", "/nonexistent/npm");
    let missing = post_plugins(
        &root,
        &configs,
        json!({
            "cwd": root.to_str().unwrap(),
            "action": "install",
            "source": "npm:pi-mcp-adapter",
            "scope": "global",
        }),
    )
    .await;
    assert_eq!(
        missing.status,
        StatusCode::INTERNAL_SERVER_ERROR,
        "{}",
        missing.text()
    );
    assert!(
        missing.json()["error"].as_str().unwrap().contains("npm"),
        "{}",
        missing.text()
    );

    std::env::remove_var("TOILER_NPM");
}

/// 系统提示词：pi 的分节结构 + `get_state.systemPrompt`。
///
/// 前端「系统提示词」面板读的就是 `state.systemPrompt`
/// （`hooks/useAgentSession.ts` 的 `setSystemPrompt(state.systemPrompt)`，
/// `AppShell.tsx` 里 `systemPrompt === null` 时渲染「系统提示词尚未加载」）。
/// 这一项以前没返回，面板永远停在未加载；提示词本身也只有一句话，
/// 模型既看不到工具清单与行为准则，也读不到项目的 AGENTS.md。
#[tokio::test]
// 串行锁跨 await 持有是故意的：这些测试都在各自的 tokio runtime 线程上 block_on，
// 持锁方不会被别人的 lock() 卡住（不会被反过来等待），而 `TOILER_AGENT_DIR` 这类
// 进程级变量必须整段互斥。
#[allow(clippy::await_holding_lock)]
async fn system_prompt_has_pi_sections_and_get_state_exposes_it() {
    let _env = env_guard();
    let root = fixture_root("prompt");
    let agent_dir = fixture_root("prompt-agent");
    let project = fixture_root("prompt-project");
    std::fs::write(
        project.join("AGENTS.md"),
        "# 项目约定\n\n改代码前先跑 cargo test。\n",
    )
    .unwrap();

    // 全局 AGENTS.md 从 toiler 自己的 agent 目录发现，这里指到临时目录，
    // 免得用户的 `~/.agents/toiler/AGENTS.md` 混进断言。
    let previous = std::env::var("TOILER_AGENT_DIR").ok();
    std::env::set_var("TOILER_AGENT_DIR", &agent_dir);

    let cwd = project.to_string_lossy().to_string();
    let dir = root.join(toiler_session::session_directory_name(&cwd));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join(toiler_session::session_file_name(1794292904431, "sess-a")),
        format!(
            "{{\"type\":\"session\",\"version\":3,\"id\":\"sess-a\",\"timestamp\":\"2026-11-10T06:41:44.431Z\",\"cwd\":{}}}\n",
            serde_json::to_string(&cwd).unwrap()
        ),
    )
    .unwrap();

    let router = app_with_two_models(&root, &project, &agent_dir);

    // 中文界面：前端打开会话就发这一条，并用返回值刷新面板
    let zh = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"set_ui_locale","locale":"zh-CN"}"#),
        &[],
    )
    .await;
    assert_eq!(zh.status, StatusCode::OK, "{}", zh.text());
    let prompt = zh.json()["data"]["systemPrompt"]
        .as_str()
        .expect("set_ui_locale 必须返回 systemPrompt")
        .to_string();
    assert!(
        prompt.starts_with("你是运行在 toiler 编程代理框架中的专业编程助手"),
        "preamble 不对：{prompt}"
    );
    assert!(prompt.contains("<tools>\n- read: 读取文件内容"), "{prompt}");
    assert!(prompt.contains("- bash: 执行 bash 命令"), "{prompt}");
    assert!(prompt.contains("<rules>\n- 使用 read 查看文件"), "{prompt}");
    assert!(prompt.contains("- 回答应简洁明了"), "{prompt}");
    assert!(
        prompt.contains(&format!("<cwd>\n{cwd}\n</cwd>")),
        "{prompt}"
    );
    assert!(prompt.contains("输出语言规则（不可违背）"), "{prompt}");
    // AGENTS.md：有 read 工具时只给路径，不内联正文
    assert!(prompt.contains("<project_context>"), "{prompt}");
    assert!(prompt.contains(&format!("- {cwd}/AGENTS.md")), "{prompt}");
    assert!(
        !prompt.contains("改代码前先跑 cargo test"),
        "有 read 时应压成路径：{prompt}"
    );

    // `get_state` 必须是同一份：面板显示得等于真正发出去的那份
    let state = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"get_state"}"#),
        &[],
    )
    .await;
    assert_eq!(state.status, StatusCode::OK, "{}", state.text());
    let reported = state.json()["data"]["systemPrompt"].clone();
    assert_eq!(reported, Value::String(prompt.clone()), "{}", state.text());

    // 切回英文：同一套结构，文案换成英文，语言指令也跟着换
    let en = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"set_ui_locale","locale":"en"}"#),
        &[],
    )
    .await;
    assert_eq!(en.status, StatusCode::OK, "{}", en.text());
    let english = en.json()["data"]["systemPrompt"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        english.starts_with("You are an expert coding assistant"),
        "{english}"
    );
    assert!(english.contains("- read: Read file contents"), "{english}");
    assert!(
        english.contains("OUTPUT LANGUAGE — NON-NEGOTIABLE"),
        "{english}"
    );

    // 工具清单：`get_tools` 是工具预设面板的数据源（`getPresetFromTools`），
    // 之前没实现时面板拿不到任何工具。
    let listed = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"get_tools"}"#),
        &[],
    )
    .await;
    assert_eq!(listed.status, StatusCode::OK, "{}", listed.text());
    let tools = listed.json()["data"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(tools.len(), 10, "{}", listed.text());
    assert!(
        tools.iter().all(|tool| tool["active"] == true),
        "新会话默认全部启用：{}",
        listed.text()
    );

    // 切到只读预设：提示词的 `<tools>` 段必须跟着变，不能还列着 bash/edit/write
    let preset = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"set_tools","toolNames":["read","grep","find","ls"]}"#),
        &[],
    )
    .await;
    assert_eq!(preset.status, StatusCode::OK, "{}", preset.text());
    let narrowed = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"get_tools"}"#),
        &[],
    )
    .await;
    let active: Vec<String> = narrowed.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["active"] == true)
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    // 桌面工具（system_time / task_progress / ask_user）不受预设控制：
    // pi 的 `set_tools` 用 `withExtensionTools` 把它们加回激活集合。
    assert_eq!(
        active,
        vec![
            "read",
            "grep",
            "find",
            "ls",
            "system_time",
            "task_progress",
            "ask_user"
        ]
    );

    let narrowed_state = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"get_state"}"#),
        &[],
    )
    .await
    .json();
    let narrowed_prompt = narrowed_state["data"]["systemPrompt"].as_str().unwrap();
    assert!(narrowed_prompt.contains("- read: Read file contents"));
    assert!(!narrowed_prompt.contains("- bash:"), "{narrowed_prompt}");
    assert!(!narrowed_prompt.contains("- edit:"), "{narrowed_prompt}");
    // `read` 还在，AGENTS.md 依旧压成路径
    assert!(narrowed_prompt.contains("<project_context>"));

    match previous {
        Some(value) => std::env::set_var("TOILER_AGENT_DIR", value),
        None => std::env::remove_var("TOILER_AGENT_DIR"),
    }
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
    std::fs::remove_dir_all(&project).ok();
}

/// 新会话按前端的工具预设初始化，且提示词里的「用 bash 做 ls/rg/find」准则
/// 只在没有 grep/find/ls 时出现（pi 的 `buildRules`）。
///
/// 前端 `ensureNewSession` 会带 `toolNames`（`getToolNamesForPreset`，默认
/// `read/bash/edit/write`）。以前 `/api/agent/new` 忽略它，会话总是 7 个工具全开，
/// 设置页选的预设形同虚设，pi 那条条件准则也永远不会出现。
#[tokio::test]
// 串行锁跨 await 持有是故意的：这些测试都在各自的 tokio runtime 线程上 block_on，
// 持锁方不会被别人的 lock() 卡住（不会被反过来等待），而 `TOILER_AGENT_DIR` 这类
// 进程级变量必须整段互斥。
#[allow(clippy::await_holding_lock)]
async fn new_session_honours_tool_names_and_bash_rule() {
    let _env = env_guard();
    let root = fixture_root("newtools");
    let agent_dir = fixture_root("newtools-agent");
    let project = fixture_root("newtools-project");
    let previous = std::env::var("TOILER_AGENT_DIR").ok();
    std::env::set_var("TOILER_AGENT_DIR", &agent_dir);
    let router = app_with_two_models(&root, &project, &agent_dir);

    let body = json!({
        "cwd": project.to_string_lossy(),
        "type": "ensure_session",
        "toolNames": ["read", "bash", "edit", "write"],
        "uiLocale": "zh-CN",
    })
    .to_string();
    let created = call(
        router.clone(),
        "POST",
        "/api/agent/new",
        Some(body.as_str()),
        &[],
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text());
    let sid = created.json()["sessionId"].as_str().unwrap().to_string();

    let listed = call(
        router.clone(),
        "POST",
        &format!("/api/agent/{sid}"),
        Some(r#"{"type":"get_tools"}"#),
        &[],
    )
    .await;
    let active: Vec<String> = listed.json()["data"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|tool| tool["active"] == true)
        .map(|tool| tool["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        active,
        vec![
            "read",
            "bash",
            "edit",
            "write",
            "system_time",
            "task_progress",
            "ask_user"
        ]
    );

    let state = call(
        router.clone(),
        "POST",
        &format!("/api/agent/{sid}"),
        Some(r#"{"type":"get_state"}"#),
        &[],
    )
    .await;
    let prompt = state.json()["data"]["systemPrompt"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(
        prompt.contains("- 使用 bash 执行 ls、rg、find 等文件操作"),
        "{prompt}"
    );
    assert!(!prompt.contains("- grep:"), "{prompt}");

    // 切到 full 预设：grep/find/ls 回来了，那条准则就必须消失（不重复给建议）
    let full = call(
        router.clone(),
        "POST",
        &format!("/api/agent/{sid}"),
        Some(
            r#"{"type":"set_tools","toolNames":["bash","read","edit","write","grep","find","ls"]}"#,
        ),
        &[],
    )
    .await;
    assert_eq!(full.status, StatusCode::OK, "{}", full.text());
    let wide = call(
        router.clone(),
        "POST",
        &format!("/api/agent/{sid}"),
        Some(r#"{"type":"get_state"}"#),
        &[],
    )
    .await;
    let wide_prompt = wide.json()["data"]["systemPrompt"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(wide_prompt.contains("- grep:"), "{wide_prompt}");
    assert!(
        !wide_prompt.contains("ls、rg、find"),
        "有专用搜索工具时不该再让模型用 bash：{wide_prompt}"
    );

    match previous {
        Some(value) => std::env::set_var("TOILER_AGENT_DIR", value),
        None => std::env::remove_var("TOILER_AGENT_DIR"),
    }
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
    std::fs::remove_dir_all(&project).ok();
}

/// `task_progress` 的任务列表要能从 transcript 恢复，并且必须出现在
/// `get_state` 的两个扩展字段里 —— 前端任务框读的就是它们
/// （`useAgentSession.ts` 的 `setExtensionStatuses` / `setExtensionWidgets`）。
///
/// pi 在 `session_start` 时从分支里的 `toolResult.details` 重建任务列表，
/// 这里往会话文件里放一条真实的 `task_progress` 结果，验证同一条恢复路径。
// 串行锁跨 await 持有是故意的（同其余改 `TOILER_AGENT_DIR` 的用例）。
#[allow(clippy::await_holding_lock)]
#[tokio::test]
async fn task_progress_restores_from_transcript_into_get_state() {
    let _env = env_guard();
    let root = fixture_root("taskprogress");
    let agent_dir = fixture_root("taskprogress-agent");
    let previous = std::env::var("TOILER_AGENT_DIR").ok();
    std::env::set_var("TOILER_AGENT_DIR", &agent_dir);

    let entry = r#"{"type":"message","id":"tp1","parentId":null,"timestamp":"2026-11-10T06:41:46.000Z","message":{"role":"toolResult","toolCallId":"call_1","toolName":"task_progress","content":[{"type":"text","text":"已完成 0/2"}],"details":{"tasks":[{"id":"a","title":"写代码","status":"in_progress"},{"id":"b","title":"跑测试","status":"pending"}]},"isError":false,"timestamp":1794292906000}}"#;
    write_session(&root, &[entry]);
    let router = app_with_two_models(&root, Path::new("/tmp/demo"), &agent_dir);

    let state = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"get_state"}"#),
        &[],
    )
    .await;
    assert_eq!(state.status, StatusCode::OK, "{}", state.text());
    let data = state.json()["data"].clone();

    let statuses = data["extensionStatuses"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(statuses.len(), 1, "{data}");
    assert_eq!(statuses[0]["key"], "task-progress");
    let text = statuses[0]["text"].as_str().unwrap();
    assert!(text.starts_with("任务 0/2"), "{text}");
    assert!(text.contains("正在执行：写代码"), "{text}");

    let widgets = data["extensionWidgets"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(widgets.len(), 1, "{data}");
    assert_eq!(widgets[0]["key"], "任务进度");
    assert_eq!(widgets[0]["placement"], "aboveEditor");
    let lines: Vec<&str> = widgets[0]["lines"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|line| line.as_str())
        .collect();
    assert_eq!(lines, vec!["已完成 0/2", "● 写代码", "○ 跑测试"]);

    // 没有提问时不该有悬挂的 UI 请求
    assert!(
        data["pendingUiRequests"].as_array().unwrap().is_empty(),
        "{data}"
    );

    match previous {
        Some(value) => std::env::set_var("TOILER_AGENT_DIR", value),
        None => std::env::remove_var("TOILER_AGENT_DIR"),
    }
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
}

/// 读一个 settings.json 文件（测试用；文件必须是合法 JSON 对象）。
fn read_settings_file(path: &Path) -> Value {
    let text =
        std::fs::read_to_string(path).unwrap_or_else(|error| panic!("读 {path:?} 失败：{error}"));
    serde_json::from_str(&text).unwrap_or_else(|error| panic!("解析 {path:?} 失败：{error}"))
}

/// 排队消息的三个消费方必须各自拿到对得上的形状。
///
/// 阶段 6 走查「排队没生效」时发现的是一串同源缺陷：
/// 1. `get_state.queuedMessages` 发的是**数量**（`usize`），而桌面端排队条
///    （`ChatInput` 的 `queuedMessages?.steering.length`）与撤回按钮都按
///    **文本数组**消费 —— `.length` 恒 `undefined`，排队条永远空白；
/// 2. `clear_queue` 没实现，前端撤回按钮拿到 500，只弹一句
///    「Failed to recall queued messages」；
/// 3. `prompt.streamingBehavior` 没进 `deliver_as`，运行中发的 follow-up
///    被当成 steer 立刻注入。
///
/// 这里守着 1 与 2 的形状：`queuedMessages` 是数组（空时也是 `[]`），
/// `clear_queue` 返回被清掉的文本而不是报错。手机端 `/api/mobile/state`
/// 的 `queue` 仍然是数量（`http_contract.rs` 的用例守着）。
#[tokio::test]
// 串行锁跨 await 持有是故意的，理由同 `new_session_honours_tool_names_and_bash_rule`。
#[allow(clippy::await_holding_lock)]
async fn queued_messages_are_text_arrays_and_clear_queue_round_trips() {
    let _env = env_guard();
    let root = fixture_root("queue-shape");
    let agent_dir = fixture_root("queue-shape-agent");
    let project = fixture_root("queue-shape-project");
    let previous = std::env::var("TOILER_AGENT_DIR").ok();
    std::env::set_var("TOILER_AGENT_DIR", &agent_dir);
    let router = app_with_two_models(&root, &project, &agent_dir);

    let body = json!({ "cwd": project.to_string_lossy(), "type": "ensure_session" }).to_string();
    let created = call(
        router.clone(),
        "POST",
        "/api/agent/new",
        Some(body.as_str()),
        &[],
    )
    .await;
    assert_eq!(created.status, StatusCode::OK, "{}", created.text());
    let sid = created.json()["sessionId"].as_str().unwrap().to_string();

    // 1) `get_state.queuedMessages`：两条队列都得是数组，不能是数字。
    let state = call(
        router.clone(),
        "POST",
        &format!("/api/agent/{sid}"),
        Some(r#"{"type":"get_state"}"#),
        &[],
    )
    .await;
    assert_eq!(state.status, StatusCode::OK, "{}", state.text());
    let data = state.json()["data"].clone();
    for key in ["steering", "followUp"] {
        assert!(
            data["queuedMessages"][key].is_array(),
            "queuedMessages.{key} 必须是文本数组（前端读 .length）：{data}"
        );
        assert!(
            data["queuedMessages"][key].as_array().unwrap().is_empty(),
            "新会话没有排队消息：{data}"
        );
    }

    // 2) `clear_queue`：空队列也返回 `{steering:[], followUp:[]}`，
    //    以前这条命令不存在，前端撤回直接 500。
    let cleared = call(
        router.clone(),
        "POST",
        &format!("/api/agent/{sid}"),
        Some(r#"{"type":"clear_queue"}"#),
        &[],
    )
    .await;
    assert_eq!(cleared.status, StatusCode::OK, "{}", cleared.text());
    let cleared = cleared.json()["data"].clone();
    assert_eq!(cleared["steering"], json!([]), "{cleared}");
    assert_eq!(cleared["followUp"], json!([]), "{cleared}");

    // 3) `/api/mobile/state.queue` 仍是数量：与桌面端形状不同是故意的。
    let mobile = call(
        router.clone(),
        "GET",
        &format!("/api/mobile/state?session={sid}"),
        None,
        &[],
    )
    .await;
    assert_eq!(mobile.status, StatusCode::OK, "{}", mobile.text());
    let mobile = mobile.json();
    assert_eq!(mobile["queue"]["steering"], 0, "{mobile}");
    assert_eq!(mobile["queue"]["followUp"], 0, "{mobile}");

    match previous {
        Some(value) => std::env::set_var("TOILER_AGENT_DIR", value),
        None => std::env::remove_var("TOILER_AGENT_DIR"),
    }
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
    std::fs::remove_dir_all(&project).ok();
}

/// 「从此处编辑」：被编辑的那条消息**连同它之后的内容**一起从分支上消失，
/// 之后重发的新消息挂在父节点下（旧消息留在旧分支）。
///
/// 前端 `MessageView` 的编辑按钮发 `navigate_tree` 时带 `hideTarget: true`。
/// 以前它和其他两个导航入口一样只把叶子移到条目**自己**上，于是列表里那条
/// 消息原样留着 —— 用户看到的是「点了编辑，但对话没变」。
#[tokio::test]
// 依赖进程级 TOILER_AGENT_DIR，理由同其他串行用例。
#[allow(clippy::await_holding_lock)]
async fn edit_from_here_hides_target_and_everything_after_it() {
    let _env = env_guard();
    let root = fixture_root("edithere");
    let agent_dir = fixture_root("edithere-agent");
    let previous = std::env::var("TOILER_AGENT_DIR").ok();
    std::env::set_var("TOILER_AGENT_DIR", &agent_dir);
    write_session(
        &root,
        &[
            r#"{"type":"message","id":"m1","parentId":null,"timestamp":"2026-11-10T06:41:45.000Z","message":{"role":"user","content":[{"type":"text","text":"第一个问题"}],"timestamp":1794292905000}}"#,
            r#"{"type":"message","id":"m2","parentId":"m1","timestamp":"2026-11-10T06:41:46.000Z","message":{"role":"assistant","content":[{"type":"text","text":"第一个回答"}],"api":"openai-completions","provider":"first","model":"model-a","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1794292906000}}"#,
            r#"{"type":"message","id":"m3","parentId":"m2","timestamp":"2026-11-10T06:41:47.000Z","message":{"role":"user","content":[{"type":"text","text":"要改的问题"}],"timestamp":1794292907000}}"#,
            r#"{"type":"message","id":"m4","parentId":"m3","timestamp":"2026-11-10T06:41:48.000Z","message":{"role":"assistant","content":[{"type":"text","text":"要丢弃的回答"}],"api":"openai-completions","provider":"first","model":"model-a","usage":{"input":1,"output":1,"cacheRead":0,"cacheWrite":0,"totalTokens":2,"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0,"total":0}},"stopReason":"stop","timestamp":1794292908000}}"#,
        ],
    );
    let router = app_with_two_models(&root, Path::new("/tmp/demo"), &agent_dir);

    // 1) 编辑 m3：叶子退到 m2，m3 与 m4 都不在分支上。
    let edited = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"navigate_tree","targetId":"m3","hideTarget":true}"#),
        &[],
    )
    .await;
    assert_eq!(edited.status, StatusCode::OK, "{}", edited.text());
    let edited = edited.json()["data"].clone();
    assert_eq!(edited["cancelled"], false, "{edited}");
    assert_eq!(edited["leafId"], "m2", "叶子应退到父节点：{edited}");

    let hidden = call(
        router.clone(),
        "GET",
        "/api/sessions/sess-a/context?leafId=m2",
        None,
        &[],
    )
    .await;
    let hidden = hidden.json();
    let texts: Vec<&str> = hidden["context"]["messages"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["content"][0]["text"].as_str().unwrap())
        .collect();
    assert_eq!(texts, vec!["第一个问题", "第一个回答"], "{hidden}");

    // 2) 对照：不带 `hideTarget` 的导航（消息树点节点）仍把叶子放在条目自己上。
    let navigated = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"navigate_tree","targetId":"m3"}"#),
        &[],
    )
    .await;
    let navigated = navigated.json()["data"].clone();
    assert_eq!(navigated["leafId"], "m3", "{navigated}");

    // 3) 编辑第一条消息：父节点是 null，叶子移到「无」。
    let root_edit = call(
        router.clone(),
        "POST",
        "/api/agent/sess-a",
        Some(r#"{"type":"navigate_tree","targetId":"m1","hideTarget":true}"#),
        &[],
    )
    .await;
    assert_eq!(root_edit.status, StatusCode::OK, "{}", root_edit.text());
    assert_eq!(
        root_edit.json()["data"]["leafId"],
        Value::Null,
        "{}",
        root_edit.text()
    );

    match previous {
        Some(value) => std::env::set_var("TOILER_AGENT_DIR", value),
        None => std::env::remove_var("TOILER_AGENT_DIR"),
    }
    std::fs::remove_dir_all(&root).ok();
    std::fs::remove_dir_all(&agent_dir).ok();
}
