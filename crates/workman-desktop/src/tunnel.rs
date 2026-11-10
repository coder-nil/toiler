//! 公网入口（Cloudflare Tunnel）管理器。
//!
//! 配置复用 `desktop-access.json` 的 `public` 分区，所以设置页改开关不需要任何
//! IPC：这个模块的管理器线程和反向代理一样，每 400ms 读一次配置文件。
//!
//! 运行态写到**单独的** `desktop-access.tunnel.json`，不写进
//! `desktop-access.runtime.json`：那个文件由 lan_proxy 的 manager 线程反复重写，
//! 两个线程各写一份会互相覆盖（lanPort 会被隧道状态挤掉）。
//!
//! 两种模式：
//!   * `quick` —— `cloudflared tunnel --url http://127.0.0.1:<port>`，随机
//!     trycloudflare 域名，**不需要登录也不需要域名**，重启即换地址。用来先跑通。
//!   * `named` —— 生成配置后 `cloudflared tunnel --config <generated> run`，
//!     固定域名；需要 `~/.cloudflared/cert.pem`（登录过）和隧道凭据。
//!
//! 两条硬约束：
//!   1. `service` 只能是 `http://127.0.0.1:<入口代理端口>`。隧道直连 Next 会绕过
//!      认证/限流/日志这唯一一道闸门，所以这里不给任何自定义 service 的入口。
//!   2. `hostname` 必须是 punycode。界面上的 Unicode 域名由 Node 侧转好再落盘，
//!      因此这里拒绝一切非 ASCII 主机名 —— 等于拒绝了"没转换过的输入"。

use serde::{Deserialize, Serialize};
use std::collections::VecDeque;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::{configure_process_group, terminate_process};

const CONFIG_POLL_INTERVAL: Duration = Duration::from_millis(400);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);
const LOG_TAIL_LINES: usize = 40;
/// 重启退避阶梯（秒）。cloudflared 起不来时不要打满 CPU。
const RESTART_BACKOFF_SECONDS: [u64; 5] = [2, 5, 15, 30, 60];
/// cloudflared 建立边缘连接时打印的标志行。
const REGISTERED_MARKER: &str = "Registered tunnel connection";

// ---------------------------------------------------------------------------
// 配置
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TunnelMode {
    Quick,
    Named,
}

impl Default for TunnelMode {
    fn default() -> Self {
        Self::Quick
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicTunnelConfig {
    pub enabled: bool,
    pub mode: TunnelMode,
    /// punycode 形式的主机名（`xn--1xa.works`）。IDNA 转换由 Node 侧完成。
    pub hostname: Option<String>,
}

/// 解析 `desktop-access.json` 里的 `public` 分区。任何异常都退回「关闭」。
pub fn parse_tunnel_config(raw: &str) -> PublicTunnelConfig {
    let Ok(value) = serde_json::from_str::<serde_json::Value>(raw) else {
        return PublicTunnelConfig::default();
    };
    let Some(public) = value.get("public") else {
        return PublicTunnelConfig::default();
    };
    let mode = match public.get("mode").and_then(serde_json::Value::as_str) {
        Some("named") => TunnelMode::Named,
        _ => TunnelMode::Quick,
    };
    let hostname = public
        .get("hostname")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| is_ascii_hostname(value))
        .map(str::to_string);
    // named 模式没有合法主机名就不能启用 —— 否则会起一个必然 404 的隧道。
    let enabled = match mode {
        TunnelMode::Quick => public.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(false),
        TunnelMode::Named => {
            public.get("enabled").and_then(serde_json::Value::as_bool).unwrap_or(false)
                && hostname.is_some()
        }
    };
    PublicTunnelConfig { enabled, mode, hostname }
}

/// 只接受 ASCII 主机名（punycode 属于 ASCII）。
pub fn is_ascii_hostname(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || !value.contains('.') {
        return false;
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    })
}

/// 隧道 id 形态：8-4-4-4-12 的十六进制。
pub fn is_tunnel_id(value: &str) -> bool {
    let groups: Vec<&str> = value.split('-').collect();
    groups.len() == 5
        && [8usize, 4, 4, 4, 12]
            .iter()
            .zip(groups.iter())
            .all(|(expected, group)| group.len() == *expected && group.chars().all(|c| c.is_ascii_hexdigit()))
}

// ---------------------------------------------------------------------------
// 命令行与配置渲染（纯函数，便于单测）
// ---------------------------------------------------------------------------

pub fn quick_args(port: u16) -> Vec<String> {
    vec![
        "tunnel".to_string(),
        "--url".to_string(),
        format!("http://127.0.0.1:{port}"),
        "--no-autoupdate".to_string(),
    ]
}

pub fn named_args(generated_config: &Path) -> Vec<String> {
    vec![
        "tunnel".to_string(),
        "--config".to_string(),
        generated_config.display().to_string(),
        "--no-autoupdate".to_string(),
        "run".to_string(),
    ]
}

/// 从 cloudflared 的输出里抓 trycloudflare 地址。
///
/// 真实输出形如 `2026-09-21T02:32:49Z INF |  https://xxx.trycloudflare.com  |`，
/// 所以按空白与竖线断词，并只认 `.trycloudflare.com`。
pub fn extract_quick_url(line: &str) -> Option<String> {
    let start = line.find("https://")?;
    let rest = &line[start..];
    let end = rest
        .find(|c: char| c.is_whitespace() || c == '|' || c == '│')
        .unwrap_or(rest.len());
    let candidate = rest[..end].trim_end_matches(|c: char| c == '.' || c == ',' || c == ')');
    if candidate.contains(".trycloudflare.com") {
        Some(candidate.to_string())
    } else {
        None
    }
}

/// cloudflared 每建立一条边缘连接都会打印这一行。
///
/// named 模式没有可解析的地址（域名是配置里给的），所以「连上了没有」必须靠这行
/// 判断 —— 否则运行态会永远停在 `starting`、界面也拿不到地址。
pub fn is_registered_line(line: &str) -> bool {
    line.contains(REGISTERED_MARKER)
}

/// 生成 named 模式的隧道配置。每次启动都按当前入口端口重写，解决端口漂移。
pub fn render_named_config(port: u16, hostname: &str, tunnel_id: &str, credentials: &Path) -> String {
    let lines = [
        "# Generated by Pi Desktop. Rewritten on every start with the current proxy port.".to_string(),
        "# The service must stay on the loopback ingress proxy: it is the only auth gate.".to_string(),
        format!("tunnel: {tunnel_id}"),
        format!("credentials-file: {}", credentials.display()),
        String::new(),
        "ingress:".to_string(),
        format!("  - hostname: {hostname}"),
        format!("    service: http://127.0.0.1:{port}"),
        "    originRequest:".to_string(),
        format!("      httpHostHeader: 127.0.0.1:{port}"),
        "# must stay false: otherwise responses are not chunked and SSE stalls".to_string(),
        "      disableChunkedEncoding: false".to_string(),
        "      connectTimeout: 30s".to_string(),
        "  - service: http_status:404".to_string(),
        String::new(),
    ];
    lines.join("\n")
}

/// 在 `~/.cloudflared` 里找隧道凭据（`<uuid>.json`）。多个时取最近修改的那个。
pub fn find_credentials(cloudflared_home: &Path) -> Option<(String, PathBuf)> {
    let entries = std::fs::read_dir(cloudflared_home).ok()?;
    let mut candidates: Vec<(SystemTime, String, PathBuf)> = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|value| value.to_str()) else {
            continue;
        };
        if !is_tunnel_id(stem) {
            continue;
        }
        let modified = entry.metadata().and_then(|meta| meta.modified()).unwrap_or(UNIX_EPOCH);
        candidates.push((modified, stem.to_string(), path));
    }
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    candidates.into_iter().next().map(|(_, id, path)| (id, path))
}

// ---------------------------------------------------------------------------
// 阻塞原因
// ---------------------------------------------------------------------------

/// 入口代理的监听端口（`desktop-access.runtime.json` 由 lan_proxy 写入）。
/// quick/named 两种模式都必须指向它，所以没有端口就不能起隧道。
///
/// 只接受**本进程**写下的记录：文件跨重启复用，旧端口可能已被别的进程占用，
/// 照用会把隧道指到 Next 上（Host 不经改写 → 手机请求全变成 403 Untrusted）。
pub fn parse_proxy_port(raw: &str, owner_pid: u32) -> Option<u16> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    if value.get("ownerPid").and_then(serde_json::Value::as_u64) != Some(u64::from(owner_pid)) {
        return None;
    }
    if value.get("listening").and_then(serde_json::Value::as_bool) != Some(true) {
        return None;
    }
    let port = value.get("lanPort").and_then(serde_json::Value::as_u64)?;
    if (1..=65535).contains(&port) {
        Some(port as u16)
    } else {
        None
    }
}

fn read_proxy_port(path: &Path) -> Option<u16> {
    parse_proxy_port(&std::fs::read_to_string(path).ok()?, std::process::id())
}

#[derive(Debug, PartialEq, Eq)]
pub enum TunnelBlock {
    /// 入口代理还没监听（手机访问开关没打开）。此时起隧道只会得到 502。
    ProxyOffline,
    /// 找不到 cloudflared 可执行文件。
    MissingBinary,
    /// named 模式但还没登录（缺 cert.pem 或隧道凭据）。
    NeedsLogin,
    /// 生成配置写盘失败。
    WriteFailed(String),
}

impl TunnelBlock {
    pub fn state(&self) -> &'static str {
        match self {
            TunnelBlock::ProxyOffline => "needs-proxy",
            TunnelBlock::MissingBinary => "missing-binary",
            TunnelBlock::NeedsLogin => "needs-login",
            TunnelBlock::WriteFailed(_) => "error",
        }
    }

    pub fn message(&self) -> String {
        match self {
            TunnelBlock::ProxyOffline => {
                "phone access is off: the ingress proxy is not listening".to_string()
            }
            TunnelBlock::MissingBinary => "cloudflared binary not found".to_string(),
            TunnelBlock::NeedsLogin => "run `cloudflared tunnel login` first".to_string(),
            TunnelBlock::WriteFailed(error) => error.clone(),
        }
    }
}

/// 从运行态文件里读出上次记录的子进程号（纯函数，便于单测）。
pub fn read_runtime_pid(raw: &str) -> Option<u32> {
    let value = serde_json::from_str::<serde_json::Value>(raw).ok()?;
    let pid = value.get("pid").and_then(serde_json::Value::as_u64)?;
    if pid == 0 || pid > u32::MAX as u64 {
        return None;
    }
    Some(pid as u32)
}

/// 回收上一次异常退出（强杀、崩溃）留下的孤儿子进程。
///
/// cloudflared 跑在自己的进程组里，父进程被杀不会带走它 —— 于是那个公网入口会
/// 一直挂着（而且下一个实例会把 `pid` 字段覆盖掉，再也找不到它）。
///
/// 只处理**确实是孤儿**的进程：PID 会被操作系统复用，所以必须同时核验命令行里有
/// `cloudflared`、且父进程已经是 1（被 launchd 收养）。别的实例还活着的子进程不动。
fn reap_orphaned_child(runtime_path: &Path) {
    let Ok(raw) = std::fs::read_to_string(runtime_path) else {
        return;
    };
    let Some(pid) = read_runtime_pid(&raw) else { return };
    if parse_process_field(&process_field(pid, "command="), false)
        .map(|command| !command.contains("cloudflared"))
        .unwrap_or(true)
    {
        return;
    }
    if parse_process_field(&process_field(pid, "ppid="), true).as_deref() != Some("1") {
        return;
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(pid as i32, libc::SIGTERM);
    }
}

/// `ps -p <pid> -o <field>` 的单行输出；进程不存在时是空字符串。
fn process_field(pid: u32, field: &str) -> String {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", field])
        .output();
    match output {
        Ok(output) => String::from_utf8_lossy(&output.stdout).trim().to_string(),
        Err(_) => String::new(),
    }
}

/// `ppid=` 的输出就是父进程号；`command=` 的输出末尾自带换行，都要先去掉。
pub fn parse_process_field(raw: &str, expect_numeric: bool) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if expect_numeric && !trimmed.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    Some(trimmed.to_string())
}

#[derive(Debug)]
enum SpawnPlan {
    Quick,
    Named { generated_config: PathBuf },
}

/// 决定这次能不能起、以及怎么起。纯逻辑之外的唯一副作用是写生成的配置。
fn plan_for(
    config: &PublicTunnelConfig,
    binary: &Path,
    cloudflared_home: &Path,
    tunnel_dir: &Path,
    port: u16,
) -> Result<SpawnPlan, TunnelBlock> {
    if !binary.is_file() {
        return Err(TunnelBlock::MissingBinary);
    }
    match config.mode {
        TunnelMode::Quick => Ok(SpawnPlan::Quick),
        TunnelMode::Named => {
            let Some(hostname) = config.hostname.as_deref() else {
                return Err(TunnelBlock::NeedsLogin);
            };
            let Some((tunnel_id, credentials)) = find_credentials(cloudflared_home) else {
                return Err(TunnelBlock::NeedsLogin);
            };
            if !cloudflared_home.join("cert.pem").is_file() {
                return Err(TunnelBlock::NeedsLogin);
            }
            let generated_config = tunnel_dir.join("cloudflared.yml");
            let rendered = render_named_config(port, hostname, &tunnel_id, &credentials);
            if let Err(error) = std::fs::write(&generated_config, rendered) {
                return Err(TunnelBlock::WriteFailed(error.to_string()));
            }
            Ok(SpawnPlan::Named { generated_config })
        }
    }
}

// ---------------------------------------------------------------------------
// 运行态
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TunnelRuntimeState {
    /// disabled | missing-binary | needs-login | starting | running | error
    pub state: String,
    pub mode: TunnelMode,
    pub url: Option<String>,
    pub hostname: Option<String>,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub last_error: Option<String>,
    pub log_tail: Vec<String>,
    pub updated_at_ms: u64,
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or(0)
}

fn write_runtime(path: &Path, state: &TunnelRuntimeState) {
    let Ok(text) = serde_json::to_string_pretty(state) else {
        return;
    };
    let _ = std::fs::write(path, text);
}

// ---------------------------------------------------------------------------
// 子进程
// ---------------------------------------------------------------------------

struct TunnelChild {
    child: Child,
    url: Arc<Mutex<Option<String>>>,
    log: Arc<Mutex<VecDeque<String>>>,
    /// 是否已建立至少一条边缘连接（named 模式唯一的「活着」信号）。
    connected: Arc<AtomicBool>,
    readers: Vec<JoinHandle<()>>,
}

fn spawn_reader<R: Read + Send + 'static>(
    source: R,
    url: Arc<Mutex<Option<String>>>,
    log: Arc<Mutex<VecDeque<String>>>,
    connected: Arc<AtomicBool>,
) -> JoinHandle<()> {
    thread::Builder::new()
        .name("public-tunnel-log".to_string())
        .spawn(move || {
            for line in BufReader::new(source).lines() {
                let Ok(line) = line else { break };
                if let Some(found) = extract_quick_url(&line) {
                    if let Ok(mut guard) = url.lock() {
                        *guard = Some(found);
                    }
                }
                if is_registered_line(&line) {
                    connected.store(true, Ordering::SeqCst);
                }
                if let Ok(mut guard) = log.lock() {
                    guard.push_back(line);
                    while guard.len() > LOG_TAIL_LINES {
                        guard.pop_front();
                    }
                }
            }
        })
        .unwrap_or_else(|_| thread::spawn(|| {}))
}

fn spawn_tunnel(binary: &Path, plan: &SpawnPlan, port: u16) -> Result<TunnelChild, String> {
    let args = match plan {
        SpawnPlan::Quick => quick_args(port),
        SpawnPlan::Named { generated_config } => named_args(generated_config),
    };
    let mut command = Command::new(binary);
    command
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .map_err(|error| format!("could not start cloudflared: {error}"))?;

    let url: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
    let log: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
    let connected = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    if let Some(stdout) = child.stdout.take() {
        readers.push(spawn_reader(
            stdout,
            Arc::clone(&url),
            Arc::clone(&log),
            Arc::clone(&connected),
        ));
    }
    if let Some(stderr) = child.stderr.take() {
        readers.push(spawn_reader(
            stderr,
            Arc::clone(&url),
            Arc::clone(&log),
            Arc::clone(&connected),
        ));
    }
    Ok(TunnelChild { child, url, log, connected, readers })
}

fn stop_child(child: &mut TunnelChild) {
    terminate_process(&mut child.child, SHUTDOWN_TIMEOUT);
    for reader in child.readers.drain(..) {
        let _ = reader.join();
    }
}

// ---------------------------------------------------------------------------
// 管理器
// ---------------------------------------------------------------------------

pub struct TunnelHandle {
    shutdown: Arc<AtomicBool>,
    manager: Option<JoinHandle<()>>,
}

impl TunnelHandle {
    pub fn stop(&mut self) {
        self.shutdown.store(true, Ordering::SeqCst);
        if let Some(manager) = self.manager.take() {
            let _ = manager.join();
        }
    }
}

impl Drop for TunnelHandle {
    fn drop(&mut self) {
        self.stop();
    }
}

/// 已起进程的标识：模式 + 域名 + 它当时指向的端口。任一项变了都要重起。
type RunningSignature = (TunnelMode, Option<String>, u16);

/// 启动隧道管理器线程。
///
/// `binary` 由调用方解析（打包后的资源目录 → 环境变量 → PATH）；隧道指向哪个
/// 端口不靠传参，而是每轮读 `proxy_runtime_path`：入口代理绑的是随机端口，
/// 端口一变就要带走隧道一起重起（见 `RunningSignature`）。
pub fn start(
    config_path: PathBuf,
    runtime_path: PathBuf,
    proxy_runtime_path: PathBuf,
    binary: PathBuf,
    tunnel_dir: PathBuf,
    cloudflared_home: PathBuf,
) -> TunnelHandle {
    let shutdown = Arc::new(AtomicBool::new(false));
    let manager_shutdown = Arc::clone(&shutdown);

    let manager = thread::Builder::new()
        .name("public-tunnel".to_string())
        .spawn(move || {
            let _ = std::fs::create_dir_all(&tunnel_dir);
            // 先清上一次留下的孤儿，避免两个 cloudflared 同时控着同一个公网入口。
            reap_orphaned_child(&runtime_path);
            let mut active: Option<TunnelChild> = None;
            let mut running_signature: Option<RunningSignature> = None;
            let mut restarts: u32 = 0;
            let mut next_attempt: Option<Instant> = None;
            let mut last_error: Option<String> = None;
            let mut last_log: Arc<Mutex<VecDeque<String>>> = Arc::new(Mutex::new(VecDeque::new()));
            let mut previous_write: Option<String> = None;

            let stop_active = |active: &mut Option<TunnelChild>, signature: &mut Option<RunningSignature>| {
                if let Some(mut current) = active.take() {
                    stop_child(&mut current);
                }
                *signature = None;
            };

            while !manager_shutdown.load(Ordering::SeqCst) {
                let raw = std::fs::read_to_string(&config_path).unwrap_or_default();
                let config = parse_tunnel_config(&raw);
                let proxy_port = read_proxy_port(&proxy_runtime_path);
                // 每轮重新算：阻塞原因不需要跨轮保留。
                let blocked: Option<TunnelBlock>;

                if !config.enabled {
                    stop_active(&mut active, &mut running_signature);
                    restarts = 0;
                    next_attempt = None;
                    last_error = None;
                    blocked = None;
                } else if let Some(port) = proxy_port {
                    let signature: RunningSignature = (config.mode, config.hostname.clone(), port);
                    // 模式、域名或端口变了：先把老的停掉，再从零开始（退避也清零）。
                    if running_signature.is_some() && running_signature.as_ref() != Some(&signature) {
                        stop_active(&mut active, &mut running_signature);
                        restarts = 0;
                        next_attempt = None;
                        last_error = None;
                    }

                    match plan_for(&config, &binary, &cloudflared_home, &tunnel_dir, port) {
                        Err(block) => {
                            stop_active(&mut active, &mut running_signature);
                            last_error = Some(block.message());
                            blocked = Some(block);
                        }
                        Ok(plan) => {
                            blocked = None;
                            if active.is_none() {
                                let due = next_attempt.map(|deadline| Instant::now() >= deadline).unwrap_or(true);
                                if due {
                                    match spawn_tunnel(&binary, &plan, port) {
                                        Ok(child) => {
                                            last_log = Arc::clone(&child.log);
                                            running_signature = Some(signature);
                                            active = Some(child);
                                            next_attempt = None;
                                            last_error = None;
                                        }
                                        Err(error) => {
                                            last_error = Some(error);
                                            restarts += 1;
                                            next_attempt = Some(Instant::now() + backoff(restarts));
                                        }
                                    }
                                }
                            } else if let Some(current) = active.as_mut() {
                                match current.child.try_wait() {
                                    Ok(Some(status)) => {
                                        last_error = Some(format!("cloudflared exited ({status})"));
                                        restarts += 1;
                                        next_attempt = Some(Instant::now() + backoff(restarts));
                                        stop_active(&mut active, &mut running_signature);
                                    }
                                    Ok(None) => {}
                                    Err(error) => last_error = Some(error.to_string()),
                                }
                            }
                        }
                    }
                } else {
                    // 代理没在监听（手机访问关着）：起了隧道也只会 502。
                    stop_active(&mut active, &mut running_signature);
                    last_error = Some(TunnelBlock::ProxyOffline.message());
                    blocked = Some(TunnelBlock::ProxyOffline);
                }

                let url = active
                    .as_ref()
                    .and_then(|child| child.url.lock().ok().and_then(|guard| guard.clone()));
                let connected = active
                    .as_ref()
                    .map(|child| child.connected.load(Ordering::SeqCst))
                    .unwrap_or(false);
                let pid = active.as_ref().map(|child| child.child.id());
                let state = if !config.enabled {
                    "disabled"
                } else if let Some(block) = blocked.as_ref() {
                    block.state()
                } else if active.is_some() {
                    // named 模式没有地址可抓，只看有没有连上边缘。
                    if url.is_some() || connected { "running" } else { "starting" }
                } else if last_error.is_some() {
                    "error"
                } else {
                    "starting"
                };

                let log_tail = last_log
                    .lock()
                    .map(|guard| guard.iter().cloned().collect::<Vec<String>>())
                    .unwrap_or_default();

                let snapshot = TunnelRuntimeState {
                    state: state.to_string(),
                    mode: config.mode,
                    url,
                    hostname: config.hostname.clone(),
                    pid,
                    restarts,
                    last_error: last_error.clone(),
                    log_tail,
                    updated_at_ms: now_ms(),
                };
                // updated_at 每次都变，所以只用「去掉时间戳」的形态判断是否需要落盘。
                let key = format!(
                    "{:?}|{:?}|{:?}|{:?}|{:?}|{:?}|{}",
                    snapshot.state,
                    snapshot.url,
                    snapshot.pid,
                    snapshot.restarts,
                    snapshot.last_error,
                    snapshot.log_tail.len(),
                    snapshot.log_tail.last().cloned().unwrap_or_default()
                );
                if previous_write.as_deref() != Some(key.as_str()) {
                    write_runtime(&runtime_path, &snapshot);
                    previous_write = Some(key);
                }

                thread::sleep(CONFIG_POLL_INTERVAL);
            }

            stop_active(&mut active, &mut running_signature);
            let final_state = TunnelRuntimeState {
                state: "disabled".to_string(),
                mode: TunnelMode::Quick,
                url: None,
                hostname: None,
                pid: None,
                restarts: 0,
                last_error: None,
                log_tail: Vec::new(),
                updated_at_ms: now_ms(),
            };
            write_runtime(&runtime_path, &final_state);
        })
        .ok();

    TunnelHandle { shutdown, manager }
}

fn backoff(attempts: u32) -> Duration {
    let index = attempts.saturating_sub(1) as usize;
    let seconds = RESTART_BACKOFF_SECONDS
        .get(index)
        .copied()
        .unwrap_or(*RESTART_BACKOFF_SECONDS.last().unwrap_or(&60));
    Duration::from_secs(seconds)
}

// ---------------------------------------------------------------------------
// 测试
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tunnel_config_defaults_to_disabled() {
        assert_eq!(parse_tunnel_config("{}"), PublicTunnelConfig::default());
        assert!(!parse_tunnel_config("not json").enabled);
        assert!(!parse_tunnel_config(r#"{"public":{}}"#).enabled);
        assert!(!parse_tunnel_config(r#"{"public":{"enabled":false,"mode":"quick"}}"#).enabled);
        // 未知 mode 落回 quick，而不是偷偷变成 named
        assert_eq!(
            parse_tunnel_config(r#"{"public":{"enabled":true,"mode":"weird"}}"#).mode,
            TunnelMode::Quick
        );
    }

    #[test]
    fn quick_mode_needs_only_the_flag() {
        let config = parse_tunnel_config(r#"{"public":{"enabled":true,"mode":"quick"}}"#);
        assert!(config.enabled);
        assert_eq!(config.mode, TunnelMode::Quick);
        assert_eq!(config.hostname, None);

        // 不写 mode 也按 quick 处理
        let bare = parse_tunnel_config(r#"{"public":{"enabled":true}}"#);
        assert!(bare.enabled);
        assert_eq!(bare.mode, TunnelMode::Quick);
    }

    #[test]
    fn named_mode_requires_a_punycode_hostname() {
        let without = parse_tunnel_config(r#"{"public":{"enabled":true,"mode":"named"}}"#);
        assert!(!without.enabled);

        let unicode = parse_tunnel_config(
            r#"{"public":{"enabled":true,"mode":"named","hostname":"π.works"}}"#,
        );
        assert!(!unicode.enabled, "未做 IDNA 转换的域名必须被拒绝");
        assert_eq!(unicode.hostname, None);

        let ok = parse_tunnel_config(
            r#"{"public":{"enabled":true,"mode":"named","hostname":"xn--1xa.works"}}"#,
        );
        assert!(ok.enabled);
        assert_eq!(ok.mode, TunnelMode::Named);
        assert_eq!(ok.hostname.as_deref(), Some("xn--1xa.works"));
    }

    #[test]
    fn never_starts_a_tunnel_without_a_listening_proxy() {
        let pid = std::process::id();
        let mine = |port: &str, listening: bool| {
            format!(r#"{{"ownerPid":{pid},"lanPort":{port},"listening":{listening}}}"#)
        };
        assert_eq!(parse_proxy_port(&mine("50169", true), pid), Some(50169));
        assert_eq!(parse_proxy_port(&mine("50169", false), pid), None);
        assert_eq!(parse_proxy_port(&mine("null", true), pid), None);
        assert_eq!(parse_proxy_port(&mine("0", true), pid), None);

        // 上一轮进程写下的端口不可信：重启后它可能已经属于 Next dev server，
        // 照用会让隧道绕过入口代理（Host 不改写 → 403 Untrusted）。
        assert_eq!(
            parse_proxy_port(r#"{"ownerPid":1,"lanPort":50169,"listening":true}"#, pid),
            None
        );
        assert_eq!(parse_proxy_port(r#"{"lanPort":50169,"listening":true}"#, pid), None);
        assert_eq!(parse_proxy_port("not json", pid), None);
        assert_eq!(parse_proxy_port("", pid), None);
    }

    #[test]
    fn hostname_validation_rejects_shapes_cloudflare_would_choke_on() {
        assert!(is_ascii_hostname("xn--1xa.works"));
        assert!(is_ascii_hostname("pi-desktop.example.com"));
        assert!(!is_ascii_hostname("π.works"));
        assert!(!is_ascii_hostname("localhost"));
        assert!(!is_ascii_hostname("-bad.example.com"));
        assert!(!is_ascii_hostname("bad-.example.com"));
        assert!(!is_ascii_hostname("a..b"));
        assert!(!is_ascii_hostname(""));
    }

    #[test]
    fn commands_point_at_the_loopback_ingress_proxy() {
        assert_eq!(
            quick_args(50169),
            vec!["tunnel", "--url", "http://127.0.0.1:50169", "--no-autoupdate"]
        );
        let named = named_args(Path::new("/tmp/cloudflared.yml"));
        assert!(named.contains(&"run".to_string()));
        assert!(named.contains(&"/tmp/cloudflared.yml".to_string()));
    }

    #[test]
    fn reads_the_recorded_child_pid_defensively() {
        assert_eq!(read_runtime_pid(r#"{"pid":92921}"#), Some(92921));
        assert_eq!(read_runtime_pid(r#"{"pid":null}"#), None);
        assert_eq!(read_runtime_pid(r#"{"pid":0}"#), None);
        assert_eq!(read_runtime_pid(r#"{"pid":"92921"}"#), None);
        assert_eq!(read_runtime_pid("not json"), None);
    }

    #[test]
    fn parses_ps_output_for_the_orphan_check() {
        assert_eq!(parse_process_field("  1\n", true).as_deref(), Some("1"));
        assert_eq!(parse_process_field("\\n", true), None);
        assert_eq!(parse_process_field("", true), None);
        assert_eq!(parse_process_field("unexpected text\n", true), None);
        assert_eq!(
            parse_process_field("/opt/homebrew/bin/cloudflared tunnel run\n", false).as_deref(),
            Some("/opt/homebrew/bin/cloudflared tunnel run")
        );
    }

    #[test]
    fn recognizes_the_edge_registration_line() {
        assert!(is_registered_line(
            "2026-09-21T06:12:13Z INF Registered tunnel connection connIndex=0 connection=41f993c7 location=lax09 protocol=quic"
        ));
        assert!(!is_registered_line("2026-09-21T06:12:04Z ERR Failed to dial a quic connection"));
        assert!(!is_registered_line(""));
    }

    #[test]
    fn extracts_the_quick_tunnel_url_from_real_output() {
        let line = "2026-09-21T02:32:49Z INF |  https://divx-richard-temperatures-payroll.trycloudflare.com  |";
        assert_eq!(
            extract_quick_url(line).as_deref(),
            Some("https://divx-richard-temperatures-payroll.trycloudflare.com")
        );
        assert_eq!(
            extract_quick_url("INF Try it at https://a-b.trycloudflare.com, ready").as_deref(),
            Some("https://a-b.trycloudflare.com")
        );
        assert_eq!(extract_quick_url("INF https://example.com"), None);
        assert_eq!(extract_quick_url("INF nothing to see"), None);
    }

    #[test]
    fn named_config_keeps_the_sse_and_gate_invariants() {
        let rendered = render_named_config(
            50169,
            "xn--1xa.works",
            "8e9d19b6-ec44-46a2-93ae-830b6e4d3829",
            Path::new("/Users/me/.cloudflared/8e9d19b6-ec44-46a2-93ae-830b6e4d3829.json"),
        );
        assert!(rendered.contains("hostname: xn--1xa.works"));
        assert!(rendered.contains("service: http://127.0.0.1:50169"));
        assert!(rendered.contains("disableChunkedEncoding: false"));
        assert!(rendered.contains("http_status:404"));
        assert!(rendered.contains("tunnel: 8e9d19b6-ec44-46a2-93ae-830b6e4d3829"));
    }

    #[test]
    fn recognizes_tunnel_credential_file_names() {
        assert!(is_tunnel_id("8e9d19b6-ec44-46a2-93ae-830b6e4d3829"));
        assert!(!is_tunnel_id("pi-desktop"));
        assert!(!is_tunnel_id("8e9d19b6-ec44-46a2-93ae"));
        assert!(!is_tunnel_id("8e9d19b6-ec44-46a2-93ae-830b6e4d3829-extra"));
    }

    #[test]
    fn backoff_grows_and_saturates() {
        assert_eq!(backoff(1), Duration::from_secs(2));
        assert_eq!(backoff(2), Duration::from_secs(5));
        assert_eq!(backoff(99), Duration::from_secs(60));
    }

    #[test]
    fn block_states_are_what_the_ui_expects() {
        assert_eq!(TunnelBlock::ProxyOffline.state(), "needs-proxy");
        assert_eq!(TunnelBlock::MissingBinary.state(), "missing-binary");
        assert_eq!(TunnelBlock::NeedsLogin.state(), "needs-login");
        assert_eq!(TunnelBlock::WriteFailed("x".to_string()).state(), "error");
    }

    fn read_state(path: &Path) -> Option<serde_json::Value> {
        serde_json::from_str(&std::fs::read_to_string(path).ok()?).ok()
    }

    fn wait_until(path: &Path, wanted: &str) -> Option<serde_json::Value> {
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(200));
            if let Some(value) = read_state(path) {
                if value.get("state").and_then(serde_json::Value::as_str) == Some(wanted) {
                    return Some(value);
                }
            }
        }
        None
    }

    /// 用一个假的 cloudflared 走完整条链：按配置起进程 → 从输出里抓地址 → 写运行态
    /// → 关掉开关后回收子进程。这是唯一能在没有桌面壳时验证管理器线程的测试。
    #[cfg(unix)]
    #[test]
    fn manager_publishes_the_quick_url_then_stops_when_disabled() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("pi-tunnel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");

        let binary = root.join("cloudflared");
        std::fs::write(
            &binary,
            "#!/bin/sh\nprintf '%s\\n' '2026-01-01T00:00:00Z INF |  https://fake-sample.trycloudflare.com  |'\nsleep 30\n",
        )
        .expect("fake binary");
        let mut permissions = std::fs::metadata(&binary).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).expect("chmod");

        let config_path = root.join("desktop-access.json");
        let proxy_runtime = root.join("desktop-access.runtime.json");
        let runtime_path = root.join("desktop-access.tunnel.json");
        std::fs::write(
            &config_path,
            r#"{"enabled":true,"password":"secret","public":{"enabled":true,"mode":"quick"}}"#,
        )
        .expect("config");
        // 没有这一份（lanPort）就不应该起隧道；ownerPid 必须是当前进程
        std::fs::write(
            &proxy_runtime,
            format!(r#"{{"ownerPid":{},"lanPort":50169,"listening":true}}"#, std::process::id()),
        )
        .expect("runtime");

        let mut handle = start(
            config_path.clone(),
            runtime_path.clone(),
            proxy_runtime.clone(),
            binary.clone(),
            root.join("tunnel"),
            root.join("cloudflared-home"),
        );

        let running = wait_until(&runtime_path, "running").expect("应该进入 running");
        assert_eq!(
            running.get("url").and_then(serde_json::Value::as_str),
            Some("https://fake-sample.trycloudflare.com")
        );
        assert!(running.get("pid").and_then(serde_json::Value::as_u64).is_some());

        // 关掉开关：子进程要被回收，运行态回到 disabled。
        std::fs::write(
            &config_path,
            r#"{"enabled":true,"password":"secret","public":{"enabled":false}}"#,
        )
        .expect("config off");
        let disabled = wait_until(&runtime_path, "disabled").expect("关闭后应该回到 disabled");
        assert!(
            disabled.get("url").and_then(serde_json::Value::as_str).is_none(),
            "disabled 时不应还挂着地址"
        );

        handle.stop();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// named 模式没有可解析的地址：运行态必须靠「Registered」标志行翻成 running，
    /// 且地址要把配置里的域名带出去（否则界面无从显示）。
    #[cfg(unix)]
    #[test]
    fn manager_reports_running_for_a_named_tunnel_without_a_url() {
        use std::os::unix::fs::PermissionsExt;

        let root = std::env::temp_dir().join(format!("pi-tunnel-named-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let cloudflared_home = root.join("cloudflared-home");
        let tunnel_dir = root.join("tunnel");
        std::fs::create_dir_all(&cloudflared_home).expect("home");
        std::fs::create_dir_all(&tunnel_dir).expect("tunnel dir");

        let binary = root.join("cloudflared");
        std::fs::write(&binary, "#!/bin/sh\nprintf '%s\\n' 'INF Registered tunnel connection connIndex=0 location=lax09'\nsleep 30\n").expect("fake binary");
        let mut permissions = std::fs::metadata(&binary).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&binary, permissions).expect("chmod");

        // named 模式要求登录证书 + 隧道凭据
        std::fs::write(cloudflared_home.join("cert.pem"), "-----BEGIN ARGO TUNNEL TOKEN-----\n\n-----END ARGO TUNNEL TOKEN-----").expect("cert");
        std::fs::write(cloudflared_home.join("8e9d19b6-ec44-46a2-93ae-830b6e4d3829.json"), "{}").expect("credentials");

        let config_path = root.join("desktop-access.json");
        let proxy_runtime = root.join("desktop-access.runtime.json");
        let runtime_path = root.join("desktop-access.tunnel.json");
        std::fs::write(
            &config_path,
            r#"{"enabled":true,"password":"secret","public":{"enabled":true,"mode":"named","hostname":"xn--1xa.works"}}"#,
        )
        .expect("config");
        std::fs::write(
            &proxy_runtime,
            format!(r#"{{"ownerPid":{},"lanPort":50402,"listening":true}}"#, std::process::id()),
        )
        .expect("runtime");

        let mut handle = start(
            config_path,
            runtime_path.clone(),
            proxy_runtime,
            binary,
            tunnel_dir.clone(),
            cloudflared_home,
        );

        let running = wait_until(&runtime_path, "running").expect("named 模式也应该进 running");
        assert!(running.get("url").and_then(serde_json::Value::as_str).is_none());
        assert_eq!(
            running.get("hostname").and_then(serde_json::Value::as_str),
            Some("xn--1xa.works")
        );

        // 生成的配置必须指回入口代理，且保留 SSE 相关的开关。
        let generated = std::fs::read_to_string(tunnel_dir.join("cloudflared.yml")).expect("generated config");
        assert!(generated.contains("service: http://127.0.0.1:50402"));
        assert!(generated.contains("hostname: xn--1xa.works"));
        assert!(generated.contains("disableChunkedEncoding: false"));

        handle.stop();
        let _ = std::fs::remove_dir_all(&root);
    }

    /// 入口代理没监听时不能起隧道：起了也只会 502。
    #[cfg(unix)]
    #[test]
    fn manager_waits_for_the_ingress_proxy_port() {
        let root = std::env::temp_dir().join(format!("pi-tunnel-wait-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("temp dir");

        let binary = root.join("cloudflared");
        std::fs::write(&binary, "#!/bin/sh\nsleep 30\n").expect("fake binary");

        let config_path = root.join("desktop-access.json");
        let proxy_runtime = root.join("desktop-access.runtime.json");
        let runtime_path = root.join("desktop-access.tunnel.json");
        std::fs::write(
            &config_path,
            r#"{"enabled":true,"password":"secret","public":{"enabled":true,"mode":"quick"}}"#,
        )
        .expect("config");
        // listening=false：代理还没起来
        std::fs::write(&proxy_runtime, r#"{"lanPort":null,"listening":false}"#).expect("runtime");

        let mut handle = start(
            config_path,
            runtime_path.clone(),
            proxy_runtime,
            binary,
            root.join("tunnel"),
            root.join("cloudflared-home"),
        );

        let blocked = wait_until(&runtime_path, "needs-proxy").expect("应报 needs-proxy");
        assert!(
            blocked.get("pid").and_then(serde_json::Value::as_u64).is_none(),
            "needs-proxy 时不应有子进程"
        );

        handle.stop();
        let _ = std::fs::remove_dir_all(&root);
    }
}
