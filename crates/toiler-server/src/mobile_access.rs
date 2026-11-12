//! 手机访问的配置读写与地址推导。
//!
//! 三份文件、两个进程的契约（与 pi 的 `lib/desktop-access.ts` 逐字段对齐）：
//!
//! | 文件 | 谁写 | 谁读 |
//! |---|---|---|
//! | `desktop-access.json` | 设置页（`PUT /api/mobile/access`） | 壳的代理与隧道管理器 |
//! | `desktop-access.runtime.json` | 壳的代理 | 这里（`lanPort`） |
//! | `desktop-access.tunnel.json` | 壳的隧道管理器 | 这里（公网地址与状态） |
//!
//! 三份分开是为了避免两个线程互相覆盖：代理每 5 秒重写运行态，隧道每变一次状态重写
//! 一次隧道态，混在一个文件里必然丢字段。
//!
//! **落盘的 hostname 永远是 punycode**：浏览器发的 `Host` 头就是 punycode，
//! cloudflared 的 ingress 按它匹配，壳那边也拒绝一切非 ASCII 主机名。Unicode 形式
//! 只出现在给人和二维码看的字段里。

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// 密码最短长度（pi 的 `MIN_DESKTOP_ACCESS_PASSWORD_LENGTH`）。
pub const MIN_PASSWORD_LENGTH: usize = 8;

/// 隧道状态机（与 pi 的 `DesktopTunnelState` 同名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TunnelState {
    Disabled,
    NeedsProxy,
    MissingBinary,
    NeedsLogin,
    Starting,
    Running,
    Error,
}

impl TunnelState {
    pub fn as_str(self) -> &'static str {
        match self {
            TunnelState::Disabled => "disabled",
            TunnelState::NeedsProxy => "needs-proxy",
            TunnelState::MissingBinary => "missing-binary",
            TunnelState::NeedsLogin => "needs-login",
            TunnelState::Starting => "starting",
            TunnelState::Running => "running",
            TunnelState::Error => "error",
        }
    }

    fn parse(raw: &str) -> Self {
        match raw {
            "needs-proxy" => TunnelState::NeedsProxy,
            "missing-binary" => TunnelState::MissingBinary,
            "needs-login" => TunnelState::NeedsLogin,
            "starting" => TunnelState::Starting,
            "running" => TunnelState::Running,
            "error" => TunnelState::Error,
            _ => TunnelState::Disabled,
        }
    }
}

/// `desktop-access.json` 的内容。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LanAccess {
    pub enabled: bool,
    pub password: Option<String>,
    pub public: PublicAccess,
}

/// 公网入口分区。
///
/// `named: false` 即 quick 模式（临时 trycloudflare 域名）；`true` 是固定域名。
/// `hostname` 永远是 punycode，或 `None`。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PublicAccess {
    pub enabled: bool,
    pub named: bool,
    pub hostname: Option<String>,
}

/// 三份文件的路径。
#[derive(Debug, Clone)]
pub struct MobilePaths {
    pub access: PathBuf,
    pub runtime: PathBuf,
    pub tunnel: PathBuf,
}

impl MobilePaths {
    pub fn in_dir(directory: &Path) -> Self {
        Self {
            access: directory.join("desktop-access.json"),
            runtime: directory.join("desktop-access.runtime.json"),
            tunnel: directory.join("desktop-access.tunnel.json"),
        }
    }
}

/// 规范化域名：转 punycode，校验标签规则。
///
/// 返回 `None` 表示不合法（调用方按「invalid-hostname」处理）。规则与 pi 的
/// `normalizeAccessHostname` 一致：必须含点、总长 ≤253、每个标签 1..=63 的
/// `[a-z0-9-]` 且不以连字符开头结尾。
pub fn normalize_hostname(value: &str) -> Option<String> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return None;
    }
    let ascii = idna::domain_to_ascii(trimmed).ok()?.to_ascii_lowercase();
    if ascii.is_empty() || !ascii.contains('.') || ascii.len() > 253 {
        return None;
    }
    let valid = ascii.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    });
    valid.then_some(ascii)
}

/// punycode → Unicode（`xn--1xa.works` → `π.works`），给人看的形式。
pub fn display_hostname(hostname: Option<&str>) -> Option<String> {
    let hostname = hostname?;
    idna::domain_to_unicode(hostname).0.into()
}

/// 读 `desktop-access.json`。坏文件一律退回「未启用」——绝不让坏配置把局域网开出来。
///
/// `enabled` 与「有密码」是**与关系**：pi 的 `readDesktopLanAccess` 里
/// `enabled: record.enabled === true && password !== null`，壳那边的
/// `parse_access_config` 也是同一套判定，两边必须一致，否则设置页显示的开关
/// 与代理实际行为会打架。
pub fn read_access(path: &Path) -> LanAccess {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return LanAccess::default();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) else {
        return LanAccess::default();
    };
    let password = map
        .get("password")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let public = map
        .get("public")
        .and_then(Value::as_object)
        .map(|public| {
            let named = public.get("mode").and_then(Value::as_str) == Some("named");
            let hostname = public
                .get("hostname")
                .and_then(Value::as_str)
                .and_then(normalize_hostname);
            // named 模式没有合法域名就不能算启用：否则会起一个必然 404 的隧道。
            let enabled = public
                .get("enabled")
                .and_then(Value::as_bool)
                .unwrap_or(false)
                && (!named || hostname.is_some());
            PublicAccess {
                enabled,
                named,
                hostname,
            }
        })
        .unwrap_or_default();

    LanAccess {
        enabled: map.get("enabled").and_then(Value::as_bool).unwrap_or(false) && password.is_some(),
        password,
        public,
    }
}

/// 写 `desktop-access.json`（0600，含父目录）。
pub fn write_access(path: &Path, access: &LanAccess) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let payload = json!({
        "enabled": access.enabled && access.password.is_some(),
        "password": access.password,
        "public": {
            "enabled": access.public.enabled,
            "mode": if access.public.named { "named" } else { "quick" },
            "hostname": access.public.hostname,
        },
    });
    let text = serde_json::to_string_pretty(&payload).unwrap_or_else(|_| "{}".to_string());
    // 先写临时文件再改名：设置页保存时不能出现「半截 JSON」，否则代理会读到坏配置。
    let temp = path.with_extension("json.tmp");
    {
        use std::io::Write;
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

/// 代理回写的运行态：只关心 `lanPort`。
///
/// `ownerPid` 校验很关键：这份文件跨进程重启复用，而端口是随机分配的。只有
/// 本实例（同一个壳进程）写下的记录才可信 —— 否则二维码会把手机指向一个已经
/// 属于别的进程（例如上一条 pi 实例）的端口。
pub fn read_lan_port(path: &Path, shell_pid: Option<u32>) -> Option<u16> {
    let raw = std::fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&raw).ok()?;
    if let Some(expected) = shell_pid {
        let owner = value.get("ownerPid").and_then(Value::as_u64);
        if owner != Some(u64::from(expected)) {
            return None;
        }
    }
    let port = value.get("lanPort").and_then(Value::as_u64)?;
    // 0 与越界都当作「还没起来」（避免二维码指向不存在的端口）。
    (1..=65535).contains(&port).then_some(port as u16)
}

/// 隧道回写的运行态。
#[derive(Debug, Clone)]
pub struct TunnelRuntime {
    pub state: TunnelState,
    pub named: bool,
    pub url: Option<String>,
    pub hostname: Option<String>,
    pub pid: Option<u32>,
    pub restarts: u32,
    pub last_error: Option<String>,
    pub log_tail: Vec<String>,
    pub updated_at_ms: Option<u64>,
}

impl Default for TunnelRuntime {
    fn default() -> Self {
        Self {
            state: TunnelState::Disabled,
            named: false,
            url: None,
            hostname: None,
            pid: None,
            restarts: 0,
            last_error: None,
            log_tail: Vec::new(),
            updated_at_ms: None,
        }
    }
}

impl TunnelRuntime {
    /// 给 `GET /api/mobile/access` 的 `tunnel` 字段。
    pub fn to_json(&self) -> Value {
        json!({
            "state": self.state.as_str(),
            "mode": if self.named { "named" } else { "quick" },
            "url": self.url,
            "hostname": self.hostname,
            "pid": self.pid,
            "restarts": self.restarts,
            "lastError": self.last_error,
            "logTail": self.log_tail,
            "updatedAtMs": self.updated_at_ms,
        })
    }
}

pub fn read_tunnel(path: &Path) -> TunnelRuntime {
    let Ok(raw) = std::fs::read_to_string(path) else {
        return TunnelRuntime::default();
    };
    let Ok(Value::Object(map)) = serde_json::from_str::<Value>(&raw) else {
        return TunnelRuntime::default();
    };
    let string_at = |key: &str| {
        map.get(key)
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|value| !value.is_empty())
    };
    TunnelRuntime {
        state: TunnelState::parse(map.get("state").and_then(Value::as_str).unwrap_or("")),
        named: map.get("mode").and_then(Value::as_str) == Some("named"),
        url: string_at("url"),
        hostname: string_at("hostname").and_then(|value| normalize_hostname(&value)),
        pid: map
            .get("pid")
            .and_then(Value::as_u64)
            .filter(|value| *value > 0)
            .map(|value| value as u32),
        restarts: map.get("restarts").and_then(Value::as_u64).unwrap_or(0) as u32,
        last_error: string_at("lastError"),
        log_tail: map
            .get("logTail")
            .and_then(Value::as_array)
            .map(|lines| {
                lines
                    .iter()
                    .filter_map(|line| line.as_str().map(str::to_string))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default(),
        updated_at_ms: map.get("updatedAtMs").and_then(Value::as_u64),
    }
}

// ============================================================================
// 局域网地址挑选
// ============================================================================

/// 地址优先级：越小越优先。家庭/公司 Wi-Fi 的私有网段排最前，其次 CGNAT
/// （Tailscale 用 100.64/10），最后是其它地址。回环与 link-local 直接排除。
pub fn lan_address_rank(address: Ipv4Addr) -> Option<u8> {
    let [first, second, _, _] = address.octets();
    if first == 127 {
        return None;
    }
    if first == 169 && second == 254 {
        return None;
    }
    match (first, second) {
        (192, 168) => Some(0),
        (10, _) => Some(1),
        (172, 16..=31) => Some(2),
        (100, 64..=127) => Some(3),
        _ => Some(4),
    }
}

/// 本机所有网卡的 IPv4（不含回环与 link-local）。
///
/// 用 `getifaddrs(3)`：pi 那边是 Node 的 `os.networkInterfaces()`，两者拿到的
/// 是同一份内核数据。这里不引入额外 crate —— 一个 FFI 就够，而且省掉一层依赖。
#[cfg(unix)]
pub fn local_ipv4_addresses() -> Vec<(String, Ipv4Addr)> {
    use std::ffi::CStr;

    let mut out = Vec::new();
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs 成功时把链表写到 head，失败返回非 0；下面按链表的
    // 官方约定遍历，最后 freeifaddrs 释放一次。
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cursor = head;
        while !cursor.is_null() {
            let entry = &*cursor;
            cursor = entry.ifa_next;
            if entry.ifa_addr.is_null() || entry.ifa_name.is_null() {
                continue;
            }
            let address = &*entry.ifa_addr;
            if address.sa_family as i32 != libc::AF_INET {
                continue;
            }
            let sockaddr = &*(entry.ifa_addr as *const libc::sockaddr_in);
            // sin_addr.s_addr 是网络字节序
            let raw = u32::from_be(sockaddr.sin_addr.s_addr);
            let ipv4 = Ipv4Addr::from(raw);
            if lan_address_rank(ipv4).is_none() {
                continue;
            }
            let name = CStr::from_ptr(entry.ifa_name).to_string_lossy().to_string();
            out.push((name, ipv4));
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(not(unix))]
pub fn local_ipv4_addresses() -> Vec<(String, Ipv4Addr)> {
    // 非 Unix 分支不做实现：toiler 当前只发布 macOS/Linux。
    // 返回空表示「拿不到局域网地址」，配对弹窗会走 no-lan-address 提示。
    Vec::new()
}

/// 挑一个手机最可能连上的地址：按优先级，再按网卡名与地址排序，保证结果稳定。
pub fn pick_lan_address() -> Option<Ipv4Addr> {
    let mut candidates: Vec<(u8, String, Ipv4Addr)> = local_ipv4_addresses()
        .into_iter()
        .filter_map(|(name, address)| lan_address_rank(address).map(|rank| (rank, name, address)))
        .collect();
    candidates.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.cmp(&b.1))
            .then_with(|| a.2.cmp(&b.2))
    });
    candidates.first().map(|(_, _, address)| *address)
}

/// 探测「本机出口地址」：连一个外部 UDP 套接字（不会真的发包），拿内核选定的源地址。
///
/// 这是 [`pick_lan_address`] 拿不到时的补充手段（例如网卡枚举不可用的平台）。
pub fn probe_outbound_address() -> Option<Ipv4Addr> {
    let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
    // 8.8.8.8:53 只是「让内核选路」的靶子；UDP connect 不发包。
    socket.connect((Ipv4Addr::new(8, 8, 8, 8), 53)).ok()?;
    match socket.local_addr().ok()?.ip() {
        IpAddr::V4(v4) if lan_address_rank(v4).is_some() => Some(v4),
        _ => None,
    }
}

// ============================================================================
// 配对地址推导
// ============================================================================

/// `url` 为 null 的原因（与 pi 的 `MobilePairReason` 同名）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PairReason {
    Ok,
    LanDisabled,
    Starting,
    LoopbackOnly,
    NoLanAddress,
}

impl PairReason {
    pub fn as_str(self) -> &'static str {
        match self {
            PairReason::Ok => "ok",
            PairReason::LanDisabled => "lan-disabled",
            PairReason::Starting => "starting",
            PairReason::LoopbackOnly => "loopback-only",
            PairReason::NoLanAddress => "no-lan-address",
        }
    }
}

/// 拼遥控页地址：`<base>/m?session=…&cwd=…`（与 pi 的 `buildMobileUrl` 一致）。
pub fn build_mobile_url(base: &str, session: Option<&str>, cwd: Option<&str>) -> String {
    let trimmed = base.trim_end_matches('/');
    let mut query: Vec<String> = Vec::new();
    if let Some(session) = session.filter(|value| !value.is_empty()) {
        // URLSearchParams 的编码：空格 → `+`，其余走百分号编码
        query.push(format!("session={}", encode_query(session)));
    }
    if let Some(cwd) = cwd.filter(|value| !value.is_empty()) {
        query.push(format!("cwd={}", encode_query(cwd)));
    }
    if query.is_empty() {
        format!("{trimmed}/m")
    } else {
        format!("{trimmed}/m?{}", query.join("&"))
    }
}

/// `URLSearchParams` 的编码规则：保留 `A-Za-z0-9-_.!~*'()`，空格写 `+`。
fn encode_query(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for byte in value.as_bytes() {
        let ch = *byte as char;
        match ch {
            'A'..='Z'
            | 'a'..='z'
            | '0'..='9'
            | '-'
            | '_'
            | '.'
            | '!'
            | '~'
            | '*'
            | '\''
            | '('
            | ')' => out.push(ch),
            ' ' => out.push('+'),
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

/// `Host` 头里的端口；没有就退到 `PI_WEB_PORT`，再退到默认 30141。
pub fn resolve_pair_port(host_header: Option<&str>, configured: Option<&str>) -> String {
    if let Some(host) = host_header {
        // 末尾 `:数字`
        let trimmed = host.trim();
        if let Some((_, port)) = trimmed.rsplit_once(':') {
            if !port.is_empty() && port.chars().all(|ch| ch.is_ascii_digit()) {
                return port.to_string();
            }
        }
    }
    configured
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| "30141".to_string())
}

/// 推导配对信息的输入。
pub struct PairInput<'a> {
    pub bind_host: &'a str,
    pub port: String,
    pub password_required: bool,
    pub desktop_shell: bool,
    /// 局域网开关已打开时的代理端口（未监听为 `None`）。
    pub lan_port: Option<u16>,
    pub lan_enabled: bool,
    pub session: Option<&'a str>,
    pub cwd: Option<&'a str>,
    /// 供测试注入；`None` 时真去枚举网卡。
    pub lan_address: Option<Option<Ipv4Addr>>,
}

/// 推导 `GET /api/mobile/pair` 的响应体。
pub fn resolve_pair(input: PairInput<'_>) -> Value {
    let lan_address = input
        .lan_address
        .unwrap_or_else(|| pick_lan_address().or_else(probe_outbound_address));

    let host_port = |host: &str, port: &str| {
        build_mobile_url(&format!("http://{host}:{port}"), input.session, input.cwd)
    };

    // 形态一：桌面壳里开了手机访问 —— 手机连的是代理的随机端口，不是内环端口。
    if input.lan_enabled {
        let Some(lan_port) = input.lan_port else {
            return pair_json(
                None,
                None,
                None,
                true,
                PairReason::Starting,
                input.password_required,
                input.desktop_shell,
            );
        };
        let Some(address) = lan_address else {
            return pair_json(
                None,
                None,
                None,
                true,
                PairReason::NoLanAddress,
                input.password_required,
                input.desktop_shell,
            );
        };
        let port = lan_port.to_string();
        let url = host_port(&address.to_string(), &port);
        return pair_json(
            Some(url),
            Some(address.to_string()),
            Some(port),
            true,
            PairReason::Ok,
            input.password_required,
            input.desktop_shell,
        );
    }

    // 形态二：命令行直启并绑了局域网地址。
    let bind_host = input.bind_host.trim();
    let bound_to_lan = !bind_host.is_empty()
        && bind_host != "127.0.0.1"
        && bind_host != "::1"
        && bind_host != "localhost";
    if !bound_to_lan {
        let reason = if input.desktop_shell {
            PairReason::LanDisabled
        } else {
            PairReason::LoopbackOnly
        };
        return pair_json(
            None,
            None,
            None,
            false,
            reason,
            input.password_required,
            input.desktop_shell,
        );
    }

    let wildcard = bind_host == "0.0.0.0" || bind_host == "::";
    let host = if wildcard {
        lan_address.map(|address| address.to_string())
    } else {
        Some(bind_host.to_string())
    };
    let Some(host) = host else {
        return pair_json(
            None,
            None,
            None,
            true,
            PairReason::NoLanAddress,
            input.password_required,
            input.desktop_shell,
        );
    };
    let url = host_port(&host, &input.port);
    pair_json(
        Some(url),
        Some(host),
        Some(input.port),
        true,
        PairReason::Ok,
        input.password_required,
        input.desktop_shell,
    )
}

fn pair_json(
    url: Option<String>,
    host: Option<String>,
    port: Option<String>,
    lan_enabled: bool,
    reason: PairReason,
    password_required: bool,
    desktop_shell: bool,
) -> Value {
    json!({
        "url": url,
        "host": host,
        "port": port,
        "lanEnabled": lan_enabled,
        "passwordRequired": password_required,
        "desktopShell": desktop_shell,
        "reason": reason.as_str(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "toiler-mobile-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn access_without_password_is_never_enabled() {
        let dir = temp_dir("nopass");
        let path = dir.join("desktop-access.json");
        std::fs::write(&path, r#"{"enabled":true,"password":null}"#).unwrap();
        assert!(!read_access(&path).enabled);

        std::fs::write(&path, r#"{"enabled":true,"password":"   "}"#).unwrap();
        assert!(!read_access(&path).enabled);

        std::fs::write(&path, "not json").unwrap();
        assert_eq!(read_access(&path), LanAccess::default());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn access_round_trips_with_owner_only_permissions() {
        let dir = temp_dir("roundtrip");
        let path = dir.join("desktop-access.json");
        let access = LanAccess {
            enabled: true,
            password: Some("12345678".to_string()),
            public: PublicAccess {
                enabled: true,
                named: true,
                hostname: Some("m.example.com".to_string()),
            },
        };
        write_access(&path, &access).unwrap();
        assert_eq!(read_access(&path), access);

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "配置里有密码，必须只给属主读");
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn named_mode_without_hostname_is_not_enabled() {
        let dir = temp_dir("named");
        let path = dir.join("desktop-access.json");
        std::fs::write(
            &path,
            r#"{"enabled":true,"password":"12345678","public":{"enabled":true,"mode":"named"}}"#,
        )
        .unwrap();
        let access = read_access(&path);
        assert!(!access.public.enabled, "named 模式没域名不能算启用");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn hostnames_normalize_to_punycode() {
        assert_eq!(
            normalize_hostname("π.works").as_deref(),
            Some("xn--1xa.works")
        );
        assert_eq!(
            normalize_hostname("M.Example.COM").as_deref(),
            Some("m.example.com")
        );
        // 没有点 / 空 / 非法标签都拒绝
        assert_eq!(normalize_hostname("localhost"), None);
        assert_eq!(normalize_hostname(""), None);
        assert_eq!(normalize_hostname("-a.example.com"), None);
        assert_eq!(normalize_hostname("a.b..c"), None);
    }

    #[test]
    fn display_hostname_returns_unicode() {
        assert_eq!(
            display_hostname(Some("xn--1xa.works")).as_deref(),
            Some("π.works")
        );
        assert_eq!(display_hostname(None), None);
    }

    #[test]
    fn lan_port_requires_matching_owner_pid() {
        let dir = temp_dir("lanport");
        let path = dir.join("desktop-access.runtime.json");

        assert_eq!(read_lan_port(&path, None), None);

        std::fs::write(&path, r#"{"lanPort":30141,"ownerPid":1234}"#).unwrap();
        assert_eq!(read_lan_port(&path, None), Some(30141));
        assert_eq!(read_lan_port(&path, Some(1234)), Some(30141));
        // 别的进程写的端口一律不认（跨重启复用文件，端口已属于别人）
        assert_eq!(read_lan_port(&path, Some(9999)), None);

        // 垃圾值当作「还没起来」
        std::fs::write(&path, r#"{"lanPort":"30141","ownerPid":1234}"#).unwrap();
        assert_eq!(read_lan_port(&path, Some(1234)), None);
        std::fs::write(&path, r#"{"lanPort":0,"ownerPid":1234}"#).unwrap();
        assert_eq!(read_lan_port(&path, Some(1234)), None);
        std::fs::write(&path, r#"{"lanPort":70000,"ownerPid":1234}"#).unwrap();
        assert_eq!(read_lan_port(&path, Some(1234)), None);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tunnel_runtime_defaults_when_missing() {
        let dir = temp_dir("tunnel");
        let runtime = read_tunnel(&dir.join("desktop-access.tunnel.json"));
        assert_eq!(runtime.state, TunnelState::Disabled);
        assert_eq!(runtime.url, None);
        assert_eq!(runtime.restarts, 0);
        assert!(runtime.log_tail.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn tunnel_runtime_parses_quick_and_named() {
        let dir = temp_dir("tunnel2");
        let path = dir.join("desktop-access.tunnel.json");
        std::fs::write(
            &path,
            r#"{"state":"running","mode":"quick","url":"https://x.trycloudflare.com",
                "pid":42,"restarts":2,"lastError":null,"logTail":["a","b"],"updatedAtMs":99}"#,
        )
        .unwrap();
        let runtime = read_tunnel(&path);
        assert_eq!(runtime.state, TunnelState::Running);
        assert!(!runtime.named);
        assert_eq!(runtime.url.as_deref(), Some("https://x.trycloudflare.com"));
        assert_eq!(runtime.pid, Some(42));
        assert_eq!(runtime.restarts, 2);
        assert_eq!(runtime.log_tail, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(runtime.updated_at_ms, Some(99));
        let json = runtime.to_json();
        assert_eq!(json["mode"], "quick");
        assert_eq!(json["state"], "running");
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn unknown_tunnel_state_falls_back_to_disabled() {
        let dir = temp_dir("tunnel3");
        let path = dir.join("desktop-access.tunnel.json");
        std::fs::write(&path, r#"{"state":"weird"}"#).unwrap();
        assert_eq!(read_tunnel(&path).state, TunnelState::Disabled);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn lan_address_ranking_prefers_private_ranges() {
        assert_eq!(lan_address_rank(Ipv4Addr::new(192, 168, 1, 5)), Some(0));
        assert_eq!(lan_address_rank(Ipv4Addr::new(10, 0, 0, 1)), Some(1));
        assert_eq!(lan_address_rank(Ipv4Addr::new(172, 16, 0, 1)), Some(2));
        assert_eq!(lan_address_rank(Ipv4Addr::new(172, 31, 255, 255)), Some(2));
        assert_eq!(lan_address_rank(Ipv4Addr::new(100, 64, 0, 1)), Some(3));
        assert_eq!(lan_address_rank(Ipv4Addr::new(8, 8, 8, 8)), Some(4));
        // 回环与 link-local 排除
        assert_eq!(lan_address_rank(Ipv4Addr::new(127, 0, 0, 1)), None);
        assert_eq!(lan_address_rank(Ipv4Addr::new(169, 254, 1, 1)), None);
        // 172.32 已不在私有段
        assert_eq!(lan_address_rank(Ipv4Addr::new(172, 32, 0, 1)), Some(4));
    }

    #[test]
    fn mobile_url_appends_path_and_query() {
        assert_eq!(build_mobile_url("http://a:1", None, None), "http://a:1/m");
        assert_eq!(
            build_mobile_url("http://a:1/", Some("sid"), None),
            "http://a:1/m?session=sid"
        );
        assert_eq!(
            build_mobile_url("http://a:1", Some("sid"), Some("/a b")),
            "http://a:1/m?session=sid&cwd=%2Fa+b"
        );
    }

    #[test]
    fn pair_port_prefers_host_header() {
        assert_eq!(resolve_pair_port(Some("127.0.0.1:8787"), None), "8787");
        assert_eq!(resolve_pair_port(Some("127.0.0.1"), Some("9000")), "9000");
        assert_eq!(resolve_pair_port(None, None), "30141");
        // 裸 IPv6 的末段会被当成端口 —— pi 的 `/:\\d+\\s*$/` 就是这个行为，
        // 实测命令行直启绑 `::1` 时二维码端口也会是 `1`。对齐而不是「修好」它。
        assert_eq!(resolve_pair_port(Some("::1"), None), "1");
    }

    #[test]
    fn pair_reports_starting_until_the_proxy_binds() {
        let info = resolve_pair(PairInput {
            bind_host: "127.0.0.1",
            port: "8787".to_string(),
            password_required: true,
            desktop_shell: true,
            lan_enabled: true,
            lan_port: None,
            session: Some("sid"),
            cwd: None,
            lan_address: Some(Some(Ipv4Addr::new(192, 168, 1, 5))),
        });
        assert_eq!(info["url"], Value::Null);
        assert_eq!(info["reason"], "starting");
        assert_eq!(info["lanEnabled"], true);
    }

    #[test]
    fn pair_uses_proxy_port_not_the_loopback_port() {
        let info = resolve_pair(PairInput {
            bind_host: "127.0.0.1",
            port: "8787".to_string(),
            password_required: true,
            desktop_shell: true,
            lan_enabled: true,
            lan_port: Some(30141),
            session: Some("sid"),
            cwd: Some("/proj"),
            lan_address: Some(Some(Ipv4Addr::new(192, 168, 1, 5))),
        });
        assert_eq!(
            info["url"],
            "http://192.168.1.5:30141/m?session=sid&cwd=%2Fproj"
        );
        assert_eq!(info["reason"], "ok");
        assert_eq!(info["port"], "30141");
    }

    #[test]
    fn pair_explains_loopback_only_for_cli_launches() {
        let cli = resolve_pair(PairInput {
            bind_host: "127.0.0.1",
            port: "8787".to_string(),
            password_required: false,
            desktop_shell: false,
            lan_enabled: false,
            lan_port: None,
            session: None,
            cwd: None,
            lan_address: Some(None),
        });
        assert_eq!(cli["reason"], "loopback-only");
        assert_eq!(cli["url"], Value::Null);

        let shell = resolve_pair(PairInput {
            bind_host: "127.0.0.1",
            port: "8787".to_string(),
            password_required: false,
            desktop_shell: true,
            lan_enabled: false,
            lan_port: None,
            session: None,
            cwd: None,
            lan_address: Some(None),
        });
        assert_eq!(shell["reason"], "lan-disabled");
    }

    #[test]
    fn pair_binds_wildcard_to_a_concrete_address() {
        let info = resolve_pair(PairInput {
            bind_host: "0.0.0.0",
            port: "8787".to_string(),
            password_required: true,
            desktop_shell: false,
            lan_enabled: false,
            lan_port: None,
            session: None,
            cwd: None,
            lan_address: Some(Some(Ipv4Addr::new(10, 1, 2, 3))),
        });
        assert_eq!(info["url"], "http://10.1.2.3:8787/m");
        assert_eq!(info["reason"], "ok");
    }

    #[test]
    fn pair_reports_no_lan_address_when_interfaces_are_empty() {
        let info = resolve_pair(PairInput {
            bind_host: "0.0.0.0",
            port: "8787".to_string(),
            password_required: false,
            desktop_shell: false,
            lan_enabled: false,
            lan_port: None,
            session: None,
            cwd: None,
            lan_address: Some(None),
        });
        assert_eq!(info["reason"], "no-lan-address");
        assert_eq!(info["url"], Value::Null);
    }

    #[test]
    fn probe_never_returns_a_loopback_address() {
        // 只断言「不返回回环」：拿得到就拿得到，拿不到就是 None。
        if let Some(address) = probe_outbound_address() {
            assert!(lan_address_rank(address).is_some(), "{address}");
        }
    }
}
