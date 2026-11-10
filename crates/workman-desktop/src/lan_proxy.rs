//! 手机访问（局域网）反向代理。
//!
//! 设计要点：Next server 永远只监听回环地址，外部请求先经这里完成 HTTP Basic
//! 认证，再改写 Host / Origin 转发给回环端口。于是：
//!
//!   * 本机窗口（走 127.0.0.1）完全不变，依旧免密；
//!   * 认证只发生在代理层，改密码只是热加载配置文件，无需重启 server，
//!     也就不会中断任何正在运行的任务；
//!   * 转发是逐字节的，SSE / 压缩响应天然可用。

use std::fmt;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const HEAD_LIMIT: usize = 64 * 1024;
const CONFIG_POLL_INTERVAL: Duration = Duration::from_millis(400);
/// 运行态文件的重写周期（即使没有流量）—— 见 manager 循环里的注释。
const RUNTIME_REFRESH_INTERVAL: Duration = Duration::from_secs(5);
const ACCEPT_POLL_INTERVAL: Duration = Duration::from_millis(50);
const CLIENT_HEAD_TIMEOUT: Duration = Duration::from_secs(20);
const COPY_BUFFER_SIZE: usize = 32 * 1024;

/// 配置文件内容（与 lib/desktop-access.ts 共享同一份 JSON）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LanAccessConfig {
    pub enabled: bool,
    pub password: Option<String>,
}

impl Default for LanAccessConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            password: None,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ProxyError {
    TooLarge,
    UnexpectedEof,
}

impl fmt::Display for ProxyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProxyError::TooLarge => write!(formatter, "request head is too large"),
            ProxyError::UnexpectedEof => {
                write!(formatter, "connection closed before the request head")
            }
        }
    }
}

/// 解析 `desktop-access.json`。任何异常都退回「未启用」，绝不因为坏配置放开局域网。
pub fn parse_access_config(raw: &str) -> LanAccessConfig {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return LanAccessConfig::default();
    };
    let password = value
        .get("password")
        .and_then(|entry| entry.as_str())
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
        .map(str::to_string);
    let enabled = value
        .get("enabled")
        .and_then(|entry| entry.as_bool())
        .unwrap_or(false)
        && password.is_some();
    LanAccessConfig { enabled, password }
}

pub fn read_access_config(path: &Path) -> LanAccessConfig {
    std::fs::read_to_string(path)
        .map(|raw| parse_access_config(&raw))
        .unwrap_or_default()
}

/// 读请求头（含空行），返回 (头字节, 已被缓冲的请求体字节)。
pub fn read_request_head<R: Read>(
    reader: &mut BufReader<R>,
) -> Result<(Vec<u8>, Vec<u8>), ProxyError> {
    let mut head = Vec::new();
    loop {
        let mut line = Vec::new();
        let read = reader
            .read_until(b'\n', &mut line)
            .map_err(|_| ProxyError::UnexpectedEof)?;
        if read == 0 {
            return Err(ProxyError::UnexpectedEof);
        }
        head.extend_from_slice(&line);
        if head.len() > HEAD_LIMIT {
            return Err(ProxyError::TooLarge);
        }
        if line == b"\r\n" || line == b"\n" {
            break;
        }
    }
    let leftover = reader.buffer().to_vec();
    Ok((head, leftover))
}

/// 大小写不敏感地取一个头的值（不含头名与 CRLF）。
pub fn header_value<'a>(head: &'a str, name: &str) -> Option<&'a str> {
    for line in head.split("\r\n").skip(1) {
        if line.is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        if key.trim().eq_ignore_ascii_case(name) {
            return Some(value.trim());
        }
    }
    None
}

/// 常量时间比较，避免通过响应时间逐字节试探密码。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}

fn base64_decode(value: &str) -> Option<Vec<u8>> {
    fn index(byte: u8) -> Option<u8> {
        match byte {
            b'A'..=b'Z' => Some(byte - b'A'),
            b'a'..=b'z' => Some(byte - b'a' + 26),
            b'0'..=b'9' => Some(byte - b'0' + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }

    let bytes: Vec<u8> = value
        .bytes()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if bytes.is_empty() || bytes.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let padding = chunk.iter().filter(|byte| **byte == b'=').count();
        if padding > 2 {
            return None;
        }
        let mut accumulator = 0u32;
        for (position, byte) in chunk.iter().enumerate() {
            let sextet = if *byte == b'=' {
                if position < 2 {
                    return None;
                }
                0
            } else {
                index(*byte)?
            };
            accumulator = (accumulator << 6) | u32::from(sextet);
        }
        out.push((accumulator >> 16) as u8);
        if padding < 2 {
            out.push((accumulator >> 8) as u8);
        }
        if padding < 1 {
            out.push(accumulator as u8);
        }
    }
    Some(out)
}

/// 解析 `Authorization: Basic ...`，返回 `(user, password)`。
pub fn decode_basic_credentials(header: &str) -> Option<(String, String)> {
    let trimmed = header.trim();
    let encoded = trimmed
        .strip_prefix("Basic ")
        .or_else(|| trimmed.strip_prefix("basic "))?;
    let decoded = base64_decode(encoded.trim())?;
    let text = String::from_utf8(decoded).ok()?;
    let (user, password) = text.split_once(':')?;
    Some((user.to_string(), password.to_string()))
}

/// 校验代理层认证：用户名固定为 `pi`，密码与当前配置常量时间比较。
pub fn is_authorized(head: &str, password: &str) -> bool {
    let Some(header) = header_value(head, "authorization") else {
        return false;
    };
    let Some((user, supplied)) = decode_basic_credentials(header) else {
        return false;
    };
    constant_time_eq(user.as_bytes(), b"pi")
        && constant_time_eq(supplied.as_bytes(), password.as_bytes())
}

/// 少数浏览器会以「不带凭据」的方式抓取公开静态资源（典型是 PWA manifest，
/// 以及安装时的图标）。它们不含任何敏感信息，放行可以避免 manifest 401
/// 与“添加到主屏幕”失败；其余路径一律需要认证。
pub fn is_public_asset_request(head: &str) -> bool {
    let Some(request_line) = head.split("\r\n").next() else {
        return false;
    };
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or("");
    let target = parts.next().unwrap_or("");
    if !method.eq_ignore_ascii_case("GET") && !method.eq_ignore_ascii_case("HEAD") {
        return false;
    }
    let path = target.split('?').next().unwrap_or(target);
    matches!(
        path,
        "/manifest.webmanifest" | "/favicon.ico" | "/sw.js" | "/offline.html"
    ) || path.starts_with("/icons/")
}

/// 改写请求头：目标是让回环上的 Next 视其为本机请求。
///
/// * `Host` / `Origin` → 回环地址（否则 origin 校验会拒绝手机来的 POST）；
/// * 丢掉 `X-Forwarded-Proto` / `X-Forwarded-Host`：Cloudflare 会写入公网方案的转发头，
///   而它们会让上面那次改写失效 —— 见下方分支里的注释；
/// * `Connection` → close（代理按「一请求一连接」转发，不做连接复用），
///   但 WebSocket 升级必须保留 `Connection: Upgrade`，否则握手直接失败；
/// * 丢掉 `Proxy-*` 与 `Keep-Alive`。
pub fn rewrite_request_head(head: &str, target_port: u16) -> String {
    let loopback = format!("127.0.0.1:{target_port}");
    // WebSocket / h2c 升级握手必须原样保留 Connection 语义。
    let upgrading = header_value(head, "upgrade").is_some();
    let mut out = String::with_capacity(head.len() + 32);
    for (position, line) in head.split("\r\n").enumerate() {
        if line.is_empty() {
            // 头结束。注意 `split` 会在结尾额外产生空串，这里必须 break ——
            // 多补一个 CRLF 会让上游一直等“下一个请求”，于是一个响应都不回。
            if position > 0 {
                out.push_str(if upgrading {
                    "Connection: Upgrade\r\n\r\n"
                } else {
                    "Connection: close\r\n\r\n"
                });
            }
            break;
        }
        if position == 0 {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        }
        let Some((key, value)) = line.split_once(':') else {
            out.push_str(line);
            out.push_str("\r\n");
            continue;
        };
        let name = key.trim();
        if name.eq_ignore_ascii_case("host") {
            out.push_str(&format!("Host: {loopback}\r\n"));
        } else if name.eq_ignore_ascii_case("origin") {
            out.push_str(&format!("Origin: http://{loopback}\r\n"));
        } else if name.eq_ignore_ascii_case("referer") {
            let rewritten = rewrite_referer(value.trim(), &loopback);
            out.push_str(&format!("Referer: {rewritten}\r\n"));
        } else if name.eq_ignore_ascii_case("x-forwarded-proto")
            || name.eq_ignore_ascii_case("x-forwarded-host")
        {
            // Cloudflare 边缘会写入 `X-Forwarded-Proto: https` / `X-Forwarded-Host: <公网域名>`。
            // 不能原样透传：Next 会拿 `X-Forwarded-Proto` 去拼 `request.url`，于是回环请求被算成
            // `https://127.0.0.1:<port>`，与上面改写后的 `Origin: http://127.0.0.1:<port>` 协议对不上，
            // 同源判定失败 —— 只带 Origin 的写操作（POST /api/agent/... 发消息）会稳定 403
            // "Untrusted API request"，而 GET / SSE 不带 Origin 所以看起来正常。
            // 本代理已经把请求伪装成回环请求，这些描述公网的转发头必须一起丢掉。
            continue;
        } else if name.eq_ignore_ascii_case("connection")
            || name.eq_ignore_ascii_case("keep-alive")
            || name.eq_ignore_ascii_case("proxy-connection")
        {
            // 由上面统一补上 `Connection: close`。
            continue;
        } else {
            out.push_str(line);
            out.push_str("\r\n");
        }
    }
    out
}

fn rewrite_referer(value: &str, loopback: &str) -> String {
    match value.split_once("://") {
        Some((_, rest)) => match rest.split_once('/') {
            Some((_, path)) => format!("http://{loopback}/{path}"),
            None => format!("http://{loopback}/"),
        },
        None => value.to_string(),
    }
}

/// `HH:MM:SS`（UTC）。std 没有日期格式化，这里只用秒级时钟，排查足够。
fn utc_clock() -> String {
    let seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|value| value.as_secs())
        .unwrap_or(0);
    let day = seconds % 86_400;
    format!("{:02}:{:02}:{:02}", day / 3600, (day % 3600) / 60, day % 60)
}

fn text_response(status_line: &str, extra_headers: &[&str], body: &str) -> Vec<u8> {
    let mut response = format!(
        "HTTP/1.1 {status_line}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n",
        body.len(),
    );
    for header in extra_headers {
        response.push_str(header);
        response.push_str("\r\n");
    }
    response.push_str("\r\n");
    response.push_str(body);
    response.into_bytes()
}

fn unauthorized_response(had_credentials: bool) -> Vec<u8> {
    // realm 里必须写出用户名：浏览器原生登录框只显示 realm，
    // 否则用户不知道要填 `pi`。
    let authenticate =
        "WWW-Authenticate: Basic realm=\"Pi Desktop (username: pi)\", charset=\"UTF-8\"";
    if had_credentials {
        text_response(
            "401 Unauthorized",
            &[authenticate],
            "用户名或密码不正确。用户名必须是 pi，密码是你在 设置 → 手机访问 里设置的那一个。\nWrong username or password — the username must be \"pi\".",
        )
    } else {
        text_response(
            "401 Unauthorized",
            &[authenticate],
            "需要访问密码：在弹出的登录框中填用户名 pi 和你设置的访问密码。\nAuthentication required: username \"pi\", password from Settings → Phone access.",
        )
    }
}

/// 失败次数（进程级）。用于拖慢暴力破解。
///
/// 退避上限故意保持很短（500ms）：认证失败时连接线程会先回响应再等待，
/// 上限太大只会把线程积压起来，对局域网暴力破解没有额外收益。
static FAILED_ATTEMPTS: AtomicUsize = AtomicUsize::new(0);
static LAST_FAILURE: Mutex<Option<std::time::Instant>> = Mutex::new(None);

fn backoff_after_failure() -> Duration {
    let now = std::time::Instant::now();
    let mut guard = LAST_FAILURE.lock().ok();
    let recent = match guard.as_deref() {
        Some(Some(previous)) => now.duration_since(*previous) < Duration::from_secs(60),
        _ => false,
    };
    if !recent {
        FAILED_ATTEMPTS.store(0, Ordering::SeqCst);
    }
    if let Some(inner) = guard.as_mut() {
        **inner = Some(now);
    }
    let attempts = FAILED_ATTEMPTS.fetch_add(1, Ordering::SeqCst) + 1;
    Duration::from_millis((50u64.saturating_mul(attempts as u64)).min(500))
}

fn clear_failures() {
    FAILED_ATTEMPTS.store(0, Ordering::SeqCst);
    if let Ok(mut guard) = LAST_FAILURE.lock() {
        *guard = None;
    }
}

/// 认证/转发计数，写到运行时文件里，便于从桌面侧确认手机到底有没有连上来。
#[derive(Debug, Default)]
struct ProxyStats {
    /// 代理日志文件（与运行时文件同目录），排查问题时直接读它。
    log_path: Mutex<Option<PathBuf>>,
    auth_successes: AtomicUsize,
    auth_failures: AtomicUsize,
    forwarded: AtomicUsize,
    last_request_bytes: AtomicUsize,
    last_response_bytes: AtomicUsize,
    last_peer: Mutex<Option<String>>,
}

impl ProxyStats {
    fn set_log_path(&self, path: PathBuf) {
        if let Ok(mut guard) = self.log_path.lock() {
            *guard = Some(path);
        }
    }

    fn log(&self, message: &str) {
        let Ok(guard) = self.log_path.lock() else {
            return;
        };
        let Some(path) = guard.as_ref() else { return };
        let stamp = utc_clock();
        let line = format!("{stamp} {message}\n");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
        {
            let _ = file.write_all(line.as_bytes());
        }
    }

    fn snapshot(&self) -> serde_json::Value {
        serde_json::json!({
            "authSuccesses": self.auth_successes.load(Ordering::Relaxed),
            "authFailures": self.auth_failures.load(Ordering::Relaxed),
            "forwarded": self.forwarded.load(Ordering::Relaxed),
            "lastRequestBytes": self.last_request_bytes.load(Ordering::Relaxed),
            "lastResponseBytes": self.last_response_bytes.load(Ordering::Relaxed),
            "lastPeer": self.last_peer.lock().ok().and_then(|guard| guard.clone()),
        })
    }
}

fn disabled_response() -> Vec<u8> {
    text_response("403 Forbidden", &[], "手机访问已关闭 / Phone access is off")
}

fn bad_gateway_response() -> Vec<u8> {
    text_response("502 Bad Gateway", &[], "Pi Desktop server is unavailable")
}

fn copy_stream<R: Read, W: Write>(mut reader: R, mut writer: W) -> usize {
    let mut buffer = vec![0u8; COPY_BUFFER_SIZE];
    let mut total = 0usize;
    loop {
        match reader.read(&mut buffer) {
            Ok(0) | Err(_) => break,
            Ok(read) => {
                if writer.write_all(&buffer[..read]).is_err() {
                    break;
                }
                if writer.flush().is_err() {
                    break;
                }
                total += read;
            }
        }
    }
    let _ = writer.flush();
    total
}

fn upstream_address(target_port: u16) -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, target_port))
}

fn forward_fixed_body<R: Read>(
    reader: &mut R,
    buffered: &[u8],
    content_length: usize,
    upstream: &mut TcpStream,
) -> std::io::Result<()> {
    let first = buffered.len().min(content_length);
    if first > 0 {
        upstream.write_all(&buffered[..first])?;
    }
    let mut remaining = content_length.saturating_sub(first);
    let mut buffer = vec![0u8; COPY_BUFFER_SIZE];
    while remaining > 0 {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "request body ended early",
            ));
        }
        let take = read.min(remaining);
        upstream.write_all(&buffer[..take])?;
        remaining -= take;
    }
    upstream.flush()
}

fn handle_client(
    client: TcpStream,
    target_port: u16,
    password: Option<String>,
    stats: Arc<ProxyStats>,
    peer: SocketAddr,
) {
    let _ = client.set_read_timeout(Some(CLIENT_HEAD_TIMEOUT));
    let mut reader = match client.try_clone() {
        Ok(clone) => BufReader::new(clone),
        Err(_) => return,
    };

    let (head_bytes, leftover) = match read_request_head(&mut reader) {
        Ok(parts) => parts,
        Err(_) => return,
    };
    let head = String::from_utf8_lossy(&head_bytes).to_string();
    if let Ok(mut guard) = stats.last_peer.lock() {
        *guard = Some(peer.to_string());
    }
    let request_line = head.split("\r\n").next().unwrap_or("").to_string();
    stats.log(&format!(
        "peer={peer} request=\"{request_line}\" bytes={}",
        head_bytes.len()
    ));

    let mut writer = client;
    let Some(password) = password else {
        let _ = writer.write_all(&disabled_response());
        return;
    };
    let provided_credentials = header_value(&head, "authorization").is_some();
    if is_public_asset_request(&head) {
        stats.log(&format!(
            "public asset peer={peer} (no credentials required)"
        ));
    } else if !is_authorized(&head, &password) {
        stats.auth_failures.fetch_add(1, Ordering::Relaxed);
        let delay = backoff_after_failure();
        stats.log(&format!(
            "auth FAILED peer={peer} has-credentials={provided_credentials} failures={}",
            stats.auth_failures.load(Ordering::Relaxed),
        ));
        let _ = writer.write_all(&unauthorized_response(provided_credentials));
        let _ = writer.flush();
        thread::sleep(delay);
        return;
    }
    clear_failures();
    stats.auth_successes.fetch_add(1, Ordering::Relaxed);
    stats.log(&format!("auth OK peer={peer}"));

    let rewritten = rewrite_request_head(&head, target_port);
    let Ok(mut upstream) = TcpStream::connect(upstream_address(target_port)) else {
        stats.log(&format!(
            "upstream connect FAILED target=127.0.0.1:{target_port}"
        ));
        let _ = writer.write_all(&bad_gateway_response());
        return;
    };
    if upstream.write_all(rewritten.as_bytes()).is_err() {
        return;
    }
    // 升级请求（Next dev 的 HMR WebSocket）在握手后就是一条长连接隧道：
    // 需要双向透传，并且不能给客户端设读超时，否则空闲几十秒就会被我们自己掐断。
    let upgrading = header_value(&head, "upgrade").is_some();
    if upgrading {
        let _ = writer.set_read_timeout(None);
    }

    let transfer_chunked = header_value(&head, "transfer-encoding")
        .map(|value| {
            value
                .split(',')
                .any(|part| part.trim().eq_ignore_ascii_case("chunked"))
        })
        .unwrap_or(false);
    let content_length =
        header_value(&head, "content-length").and_then(|value| value.parse::<usize>().ok());

    let redacted = rewritten
        .split("\r\n")
        .map(|line| {
            if line.to_ascii_lowercase().starts_with("authorization:") {
                "Authorization: ***".to_string()
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\\r\\n");
    stats.log(&format!(
        "upstream ok target=127.0.0.1:{target_port} head_bytes={} leftover={} chunked={transfer_chunked} length={content_length:?}",
        rewritten.len(),
        leftover.len(),
    ));
    stats.log(&format!("forward head={redacted}"));

    // 绝不要对上游 shutdown(Write)：FIN 会让 Next 直接断开、一个字节都不回
    // （见 tests::never_half_closes_the_upstream）。HTTP/1.1 里 GET 在头结束时
    // 就已完整，带 Content-Length 的 POST 在 body 写完后也已完整。
    if let Some(length) = content_length {
        if forward_fixed_body(&mut reader, &leftover, length, &mut upstream).is_err() {
            let _ = writer.write_all(&bad_gateway_response());
            return;
        }
    } else if transfer_chunked {
        // chunked 请求体由客户端自己发结束块，代理只需继续透传。
        if !leftover.is_empty() && upstream.write_all(&leftover).is_err() {
            return;
        }
        let _ = upstream.flush();
    }

    // 请求体与响应体都逐字节转发：SSE 会一直挂着，直到任一端断开。
    stats.forwarded.fetch_add(1, Ordering::Relaxed);
    stats
        .last_request_bytes
        .store(rewritten.len() + leftover.len(), Ordering::Relaxed);
    let uploader = if transfer_chunked || upgrading {
        let Ok(mut upstream_writer) = upstream.try_clone() else {
            return;
        };
        let Ok(mut client_reader) = writer.try_clone() else {
            return;
        };
        Some(thread::spawn(move || {
            copy_stream(&mut client_reader, &mut upstream_writer);
        }))
    } else {
        None
    };
    let response_bytes = copy_stream(&mut upstream, &mut writer);
    stats
        .last_response_bytes
        .store(response_bytes, Ordering::Relaxed);
    stats.log(&format!("done peer={peer} response_bytes={response_bytes}"));
    let _ = writer.shutdown(std::net::Shutdown::Both);
    if let Some(uploader) = uploader {
        let _ = uploader.join();
    }
}

/// 直接向内核要一个空闲端口（端口 0），并避开上游 server 自己的端口。
///
/// 不用固定端口：桌面壳的 Next server 也会自己挑端口，两边都写死就容易撞车。
/// 实际端口会回写到运行时文件，二维码始终指向真实地址。
fn bind_listener(excluded_port: u16) -> Option<(TcpListener, u16)> {
    for _ in 0..8 {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        let port = listener.local_addr().ok()?.port();
        if port != excluded_port {
            return Some((listener, port));
        }
    }
    None
}

fn write_runtime_port(path: &Path, port: Option<u16>, stats: Option<&ProxyStats>) {
    // `ownerPid` 很关键：这份运行态文件会**跨进程重启复用**，而端口是随机分配、
    // 重启后会变。没有它，隧道管理器会在新代理还没绑定之前读到上一轮的旧端口 ——
    // 那个端口届时可能已经被别的进程（比如 Next dev server）占住，于是隧道直接
    // 指向 Next，Host 不经改写，手机请求全变成 403 Untrusted。
    let mut payload = serde_json::json!({
        "lanPort": port,
        "listening": port.is_some(),
        "ownerPid": std::process::id(),
    });
    if let (Some(entry), Some(stats)) = (payload.as_object_mut(), stats) {
        if let Some(fields) = stats.snapshot().as_object() {
            for (key, value) in fields {
                entry.insert(key.clone(), value.clone());
            }
        }
    }
    let _ = std::fs::write(path, payload.to_string());
}

fn spawn_acceptor(
    listener: TcpListener,
    target_port: u16,
    auth: Arc<Mutex<Option<String>>>,
    stats: Arc<ProxyStats>,
) -> (TcpListener, Arc<AtomicBool>) {
    let _ = listener.set_nonblocking(true);
    let flag = Arc::new(AtomicBool::new(false));
    let accept_flag = Arc::clone(&flag);
    if let Ok(accept_listener) = listener.try_clone() {
        let _ = thread::Builder::new()
            .name("lan-proxy-accept".to_string())
            .spawn(move || {
                while !accept_flag.load(Ordering::SeqCst) {
                    match accept_listener.accept() {
                        Ok((stream, peer)) => {
                            let auth = Arc::clone(&auth);
                            let stats = Arc::clone(&stats);
                            // 用 Builder 而不是 thread::spawn：线程创建失败时不能把 accept 循环带走。
                            let spawned = thread::Builder::new()
                                .name("lan-proxy-conn".to_string())
                                .spawn(move || {
                                    // 每个连接都取当下的密码，改密码无需重启监听。
                                    let password = auth.lock().ok().and_then(|guard| guard.clone());
                                    handle_client(stream, target_port, password, stats, peer);
                                });
                            if spawned.is_err() {
                                continue;
                            }
                        }
                        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(ACCEPT_POLL_INTERVAL);
                        }
                        Err(_) => thread::sleep(ACCEPT_POLL_INTERVAL),
                    }
                }
            });
    }
    (listener, flag)
}

struct ActiveListener {
    flag: Arc<AtomicBool>,
    #[allow(dead_code)]
    listener: TcpListener,
}

/// 代理句柄：留着它就能在应用退出时干净地停掉监听线程。
pub struct LanProxyHandle {
    shutdown: Arc<AtomicBool>,
    manager: Option<JoinHandle<()>>,
}

impl LanProxyHandle {
    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(manager) = self.manager.take() {
            let _ = manager.join();
        }
    }
}

impl Drop for LanProxyHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 启动代理管理器线程。
///
/// 它按配置文件启停监听：`enabled` 且有密码 → 监听 0.0.0.0；否则关闭监听。
/// 配置文件变化通过 mtime 检测，所以改密码即时生效。
pub fn start(config_path: PathBuf, runtime_path: PathBuf, target_port: u16) -> LanProxyHandle {
    let shutdown = Arc::new(AtomicBool::new(false));
    let manager_shutdown = Arc::clone(&shutdown);

    let manager = thread::spawn(move || {
        let mut active: Option<ActiveListener> = None;
        // 密码放在共享单元里：acceptor 每个连接都读当下值，因此改密码立即生效。
        let auth: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
        let stats: Arc<ProxyStats> = Arc::new(ProxyStats::default());
        stats.set_log_path(runtime_path.with_file_name("desktop-access.log"));
        stats.log(&format!(
            "manager started target=127.0.0.1:{target_port} config={}",
            config_path.display()
        ));
        let mut last_stats: Option<String> = None;
        let mut last_runtime_write = Instant::now();

        while !manager_shutdown.load(Ordering::SeqCst) {
            // 配置文件只有几十字节，每次轮询直接读，避免依赖 mtime 精度。
            let config = read_access_config(&config_path);
            if let Ok(mut guard) = auth.lock() {
                *guard = config.password.clone();
            }

            let want_listening = config.enabled && config.password.is_some();
            if want_listening && active.is_none() {
                if let Some((listener, port)) = bind_listener(target_port) {
                    let (listener, flag) = spawn_acceptor(
                        listener,
                        target_port,
                        Arc::clone(&auth),
                        Arc::clone(&stats),
                    );
                    write_runtime_port(&runtime_path, Some(port), Some(&stats));
                    active = Some(ActiveListener { flag, listener });
                }
            } else if !want_listening {
                if let Some(current) = active.take() {
                    current.flag.store(true, Ordering::SeqCst);
                    stats.log("listener stopped (phone access disabled)");
                    write_runtime_port(&runtime_path, None, Some(&stats));
                }
            } else if want_listening {
                // 监听中：只在计数变化时重写，避免高频写盘；但至少每 5 秒重写一次 ——
                // 这份文件是共享的，另一个实例（或退出的旧实例）可能把它覆盖成别人的
                // 记录，而隧道管理器只认本进程写下的记录，不重写就会被「抢走」。
                let snapshot = stats.snapshot().to_string();
                if last_stats.as_deref() != Some(snapshot.as_str())
                    || last_runtime_write.elapsed() >= RUNTIME_REFRESH_INTERVAL
                {
                    let port = active
                        .as_ref()
                        .and_then(|_| read_runtime_port(&runtime_path));
                    write_runtime_port(&runtime_path, port, Some(&stats));
                    last_stats = Some(snapshot);
                    last_runtime_write = Instant::now();
                }
            }

            thread::sleep(CONFIG_POLL_INTERVAL);
        }

        if let Some(current) = active.take() {
            current.flag.store(true, Ordering::SeqCst);
        }
        write_runtime_port(&runtime_path, None, Some(&stats));
    });

    LanProxyHandle {
        shutdown,
        manager: Some(manager),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HEAD: &str = "POST /api/agent/abc HTTP/1.1\r\nHost: 192.168.1.20:30141\r\nOrigin: http://192.168.1.20:30141\r\nContent-Type: application/json\r\nAuthorization: Basic cGk6c2VjcmV0LXBhc3M=\r\nProxy-Connection: keep-alive\r\n\r\n";

    #[test]
    fn parses_enabled_config_but_requires_password() {
        assert_eq!(
            parse_access_config(r#"{"enabled":true,"password":"secret-pass"}"#),
            LanAccessConfig {
                enabled: true,
                password: Some("secret-pass".to_string())
            },
        );
        // 没有密码就绝不能算启用。
        assert_eq!(
            parse_access_config(r#"{"enabled":true,"password":""}"#).enabled,
            false
        );
        assert_eq!(parse_access_config("not json").enabled, false);
    }

    #[test]
    fn decodes_basic_credentials() {
        let (user, password) = decode_basic_credentials("Basic cGk6c2VjcmV0LXBhc3M=").unwrap();
        assert_eq!(user, "pi");
        assert_eq!(password, "secret-pass");
        assert!(decode_basic_credentials("Bearer token").is_none());
    }

    #[test]
    fn authorizes_only_the_pi_user_with_the_right_password() {
        assert!(is_authorized(HEAD, "secret-pass"));
        assert!(!is_authorized(HEAD, "secret-pas"));
        assert!(!is_authorized(HEAD, ""));
        assert!(!is_authorized(
            "GET / HTTP/1.1\r\nHost: x\r\n\r\n",
            "secret-pass"
        ));
    }

    #[test]
    fn rewrites_host_and_origin_to_the_loopback_upstream() {
        let rewritten = rewrite_request_head(HEAD, 48000);
        assert!(rewritten.contains("Host: 127.0.0.1:48000\r\n"));
        assert!(rewritten.contains("Origin: http://127.0.0.1:48000\r\n"));
        // 保留其余头，避免破坏请求语义。
        assert!(rewritten.contains("Content-Type: application/json\r\n"));
        assert!(!rewritten.contains("Proxy-Connection"));
    }

    #[test]
    fn drops_forwarded_scheme_headers_so_the_loopback_origin_still_matches() {
        // 隧道场景下 Cloudflare 会带上这几个头。X-Forwarded-Proto 的 https 会让 Next 把
        // request.url 算成 https://127.0.0.1:<port>，与改写后的 http Origin 不匹配，
        // 于是手机发消息稳定 403 Untrusted API request。
        let tunneled = "POST /api/agent/abc HTTP/1.1\r\nHost: xn--1xa.works\r\nOrigin: https://xn--1xa.works\r\nX-Forwarded-Proto: https\r\nX-Forwarded-Host: xn--1xa.works\r\nCf-Connecting-Ip: 240e:46d::1\r\nContent-Type: application/json\r\n\r\n";
        let rewritten = rewrite_request_head(tunneled, 48000);
        assert!(
            !rewritten.to_ascii_lowercase().contains("x-forwarded-proto"),
            "{rewritten:?}"
        );
        assert!(
            !rewritten.to_ascii_lowercase().contains("x-forwarded-host"),
            "{rewritten:?}"
        );
        assert!(rewritten.contains("Host: 127.0.0.1:48000\r\n"));
        assert!(rewritten.contains("Origin: http://127.0.0.1:48000\r\n"));
        // 判定「同网还是公网」要用它，不能丢。
        assert!(rewritten.contains("Cf-Connecting-Ip: 240e:46d::1\r\n"));
    }

    #[test]
    fn allows_only_public_static_assets_without_credentials() {
        // 浏览器抓 manifest / 图标时可能不带凭据。
        assert!(is_public_asset_request(
            "GET /manifest.webmanifest HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
        assert!(is_public_asset_request(
            "GET /icons/icon-192.png HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
        assert!(is_public_asset_request(
            "GET /favicon.ico HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
        // 其余一律仍需认证。
        assert!(!is_public_asset_request(
            "GET /m HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
        assert!(!is_public_asset_request(
            "GET /api/mobile/state HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
        assert!(!is_public_asset_request(
            "POST /manifest.webmanifest HTTP/1.1\r\nHost: x\r\n\r\n"
        ));
    }

    #[test]
    fn keeps_the_upgrade_handshake_alive_for_websockets() {
        // Next dev 的 HMR 会走 WebSocket：把 Connection 改成 close 会让握手直接失败。
        let upgrade_head = "GET /_next/hmr?id=abc HTTP/1.1\r\nHost: 192.168.1.20:30141\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Key: dGVzdA==\r\n\r\n";
        let rewritten = rewrite_request_head(upgrade_head, 48000);
        assert!(
            rewritten.contains("Upgrade: websocket\r\n"),
            "{rewritten:?}"
        );
        assert!(
            rewritten.contains("Connection: Upgrade\r\n"),
            "{rewritten:?}"
        );
        assert!(!rewritten.contains("Connection: close"), "{rewritten:?}");
        assert!(rewritten.ends_with("\r\n\r\n"), "{rewritten:?}");

        // 普通请求仍然强制 close，避免上游保持连接。
        let plain = rewrite_request_head(HEAD, 48000);
        assert!(plain.contains("Connection: close\r\n"), "{plain:?}");
    }

    #[test]
    fn terminates_the_head_exactly_once_and_forces_close() {
        let rewritten = rewrite_request_head(HEAD, 48000);
        // 头必须以单个空行结束：多一个 CRLF，上游就会一直等下一个请求，一个响应都不回。
        assert!(
            rewritten.ends_with("Connection: close\r\n\r\n"),
            "{rewritten:?}"
        );
        assert_eq!(rewritten.matches("\r\n\r\n").count(), 1, "{rewritten:?}");
        assert_eq!(
            rewritten.matches("Connection: close").count(),
            1,
            "{rewritten:?}"
        );
    }

    #[test]
    fn reads_the_head_and_keeps_the_buffered_body() {
        let payload = format!("{HEAD}\"{{\\\"type\\\":\\\"prompt\\\"}}\"");
        let mut reader = BufReader::new(payload.as_bytes());
        let (head, leftover) = read_request_head(&mut reader).unwrap();
        assert!(String::from_utf8_lossy(&head).starts_with("POST /api/agent/abc"));
        assert!(!leftover.is_empty());
    }

    #[test]
    fn responses_report_their_own_length() {
        for response in [
            unauthorized_response(false),
            unauthorized_response(true),
            disabled_response(),
        ] {
            let response = String::from_utf8(response).unwrap();
            let (head, body) = response.split_once("\r\n\r\n").unwrap();
            let declared: usize = head
                .lines()
                .find_map(|line| line.strip_prefix("Content-Length: "))
                .unwrap()
                .trim()
                .parse()
                .unwrap();
            assert_eq!(declared, body.len());
        }
    }

    #[test]
    fn the_challenge_tells_the_user_which_username_to_use() {
        // 浏览器原生登录框只显示 realm，忘记写用户名会让用户无从下手。
        let challenge = String::from_utf8(unauthorized_response(false)).unwrap();
        assert!(
            challenge.contains("realm=\"Pi Desktop (username: pi)\""),
            "{challenge}"
        );
        assert!(challenge.contains("username"), "{challenge}");
    }
}

#[cfg(test)]
mod integration_tests {
    use super::*;
    use std::sync::Mutex;

    /// 极简上游：把收到的请求头存起来，回一个固定响应。
    fn spawn_upstream() -> (u16, Arc<Mutex<Vec<String>>>) {
        let listener = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let received: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&received);
        thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { break };
                let Ok(clone) = stream.try_clone() else {
                    continue;
                };
                let mut reader = BufReader::new(clone);
                if let Ok((head, _)) = read_request_head(&mut reader) {
                    sink.lock()
                        .unwrap()
                        .push(String::from_utf8_lossy(&head).to_string());
                    let body = "upstream-ok";
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len(),
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            }
        });
        (port, received)
    }

    fn workspace(name: &str) -> PathBuf {
        let directory = std::env::temp_dir().join(format!(
            "pi-desktop-lan-proxy-{name}-{}-{:?}",
            std::process::id(),
            thread::current().id(),
        ));
        let _ = std::fs::remove_dir_all(&directory);
        std::fs::create_dir_all(&directory).unwrap();
        directory
    }

    fn write_config(path: &Path, enabled: bool, password: &str) {
        std::fs::write(
            path,
            format!("{{\"enabled\":{enabled},\"password\":\"{password}\"}}"),
        )
        .unwrap();
    }

    fn wait_for_port(runtime_path: &Path) -> u16 {
        for _ in 0..120 {
            if let Ok(raw) = std::fs::read_to_string(runtime_path) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
                    if let Some(port) = value.get("lanPort").and_then(|entry| entry.as_u64()) {
                        return port as u16;
                    }
                }
            }
            thread::sleep(Duration::from_millis(50));
        }
        panic!("proxy never reported a listening port");
    }

    fn call(port: u16, authorization: Option<&str>) -> String {
        let mut stream = TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).unwrap();
        let auth = authorization
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        let request = format!("GET /m HTTP/1.1\r\nHost: 192.168.1.20:{port}\r\nOrigin: http://192.168.1.20:{port}\r\n{auth}Connection: close\r\n\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        let mut response = String::new();
        let _ = stream.read_to_string(&mut response);
        response
    }

    const GOOD: &str = "Basic cGk6bG9uZy1lbm91Z2gtc2VjcmV0";
    const WRONG: &str = "Basic cGk6d3JvbmctcGFzc3dvcmQ=";

    #[test]
    fn authenticates_requests_and_rewrites_them_for_the_loopback_upstream() {
        let directory = workspace("auth");
        let config_path = directory.join("desktop-access.json");
        let runtime_path = directory.join("desktop-access.runtime.json");
        write_config(&config_path, true, "long-enough-secret");

        let (upstream_port, received) = spawn_upstream();
        let mut proxy = start(config_path.clone(), runtime_path.clone(), upstream_port);
        let port = wait_for_port(&runtime_path);

        // 没有凭据 → 401，且请求不会到达上游。
        let unauthorized = call(port, None);
        assert!(unauthorized.starts_with("HTTP/1.1 401"), "{unauthorized}");
        assert!(received.lock().unwrap().is_empty());

        let wrong = call(port, Some(WRONG));
        assert!(wrong.starts_with("HTTP/1.1 401"), "{wrong}");

        let authorized = call(port, Some(GOOD));
        assert!(authorized.starts_with("HTTP/1.1 200"), "{authorized}");
        assert!(authorized.contains("upstream-ok"));

        let forwarded = received.lock().unwrap().clone();
        assert_eq!(forwarded.len(), 1);
        // 转发时必须让上游以为是本机请求，否则 Next 的 origin 校验会拒绝。
        assert!(
            forwarded[0].contains(&format!("Host: 127.0.0.1:{upstream_port}\r\n")),
            "{}",
            forwarded[0]
        );
        assert!(
            forwarded[0].contains(&format!("Origin: http://127.0.0.1:{upstream_port}\r\n")),
            "{}",
            forwarded[0]
        );

        write_config(&config_path, false, "long-enough-secret");
        thread::sleep(CONFIG_POLL_INTERVAL * 4);
        proxy.stop();
        let _ = std::fs::remove_dir_all(&directory);
    }

    /// 真实 Node 上游 + 真实代理。除了断言能拿到响应，还会记录“上游是否在响应前
    /// 收到 FIN”：代理一旦对上游 shutdown(Write)，Next 就会直接断开、返回空响应。
    #[test]
    fn never_half_closes_the_upstream() {
        let directory = workspace("half-close");
        let capture = directory.join("fin.json");
        let probe = TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0)).unwrap();
        let upstream_port = probe.local_addr().unwrap().port();
        drop(probe);

        let script = format!(
            "const fs=require('fs');require('http').createServer((req,res)=>{{let fin=false;req.socket.on('end',()=>{{fin=true}});setTimeout(()=>{{fs.writeFileSync({capture},JSON.stringify({{finBeforeResponse:fin}}));res.writeHead(200,{{'content-type':'text/plain'}});res.end('node-ok:'+req.url)}},150)}}).listen({port},'127.0.0.1')",
            capture = format!("{:?}", capture.to_string_lossy()),
            port = upstream_port,
        );
        let mut child = std::process::Command::new("node")
            .arg("-e")
            .arg(script)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("node is required for this test");
        for _ in 0..60 {
            if TcpStream::connect(SocketAddrV4::new(Ipv4Addr::LOCALHOST, upstream_port)).is_ok() {
                break;
            }
            thread::sleep(Duration::from_millis(50));
        }

        let config_path = directory.join("desktop-access.json");
        let runtime_path = directory.join("desktop-access.runtime.json");
        write_config(&config_path, true, "long-enough-secret");
        let mut proxy = start(config_path, runtime_path.clone(), upstream_port);
        let port = wait_for_port(&runtime_path);

        let response = call(port, Some(GOOD));
        let record = std::fs::read_to_string(&capture).unwrap_or_else(|_| "{}".to_string());
        let _ = child.kill();
        proxy.stop();
        let _ = std::fs::remove_dir_all(&directory);

        assert!(response.starts_with("HTTP/1.1 200"), "{response:?}");
        assert!(response.contains("node-ok:/m"), "{response:?}");
        assert!(
            record.contains("\"finBeforeResponse\":false"),
            "代理在半关闭上游，Next 会因此返回空响应：{record}",
        );
    }

    #[test]
    fn picks_up_a_password_change_without_restarting() {
        let directory = workspace("hot-reload");
        let config_path = directory.join("desktop-access.json");
        let runtime_path = directory.join("desktop-access.runtime.json");
        write_config(&config_path, true, "long-enough-secret");

        let (upstream_port, _) = spawn_upstream();
        let mut proxy = start(config_path.clone(), runtime_path.clone(), upstream_port);
        let port = wait_for_port(&runtime_path);
        assert!(call(port, Some(GOOD)).starts_with("HTTP/1.1 200"));

        // 换一个密码：文件改动后立刻生效，无需重启任何进程。
        write_config(&config_path, true, "another-long-secret");
        let mut current = String::new();
        for _ in 0..40 {
            thread::sleep(Duration::from_millis(50));
            current = call(port, Some(GOOD));
            if current.starts_with("HTTP/1.1 401") {
                break;
            }
        }
        assert!(current.starts_with("HTTP/1.1 401"), "{current}");
        let new_password = base64_encode(b"pi:another-long-secret");
        assert!(call(port, Some(&format!("Basic {new_password}"))).starts_with("HTTP/1.1 200"));

        proxy.stop();
        let _ = std::fs::remove_dir_all(&directory);
    }

    fn base64_encode(value: &[u8]) -> String {
        const ALPHABET: &[u8; 64] =
            b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in value.chunks(3) {
            let first = chunk[0];
            let second = chunk.get(1).copied().unwrap_or(0);
            let third = chunk.get(2).copied().unwrap_or(0);
            out.push(ALPHABET[(first >> 2) as usize] as char);
            out.push(ALPHABET[(((first & 0b0000_0011) << 4) | (second >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPHABET[(((second & 0b0000_1111) << 2) | (third >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(ALPHABET[(third & 0b0011_1111) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }
}

/// 从运行时文件里读回当前监听端口（供管理器重写统计时沿用）。
fn read_runtime_port(path: &Path) -> Option<u16> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&raw).ok()?;
    value.get("lanPort")?.as_u64().map(|port| port as u16)
}
