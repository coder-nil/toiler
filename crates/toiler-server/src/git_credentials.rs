//! Git 凭据的**加密存储**（对齐 pi-desktop 的 `git-credentials.ts`）。
//!
//! 历史问题：toiler 早先把「记住凭据」整个交给 `git credential approve`，
//! 而 `GET /api/git` 又把 `hasSavedCredential` 硬编码成 `false`。前者只对
//! HTTPS 生效（SSH 私钥口令没有 credential helper 这一层），后者让前端在每次
//! 远程动作之后把口令框按「没存过」清成空串 —— 于是勾了「在此设备上加密记住
//! 凭据」，口令照样被清空，下次还得重打。现在按 pi 的格式自己存一份加密凭据，
//! 面板的「已保存」状态与「留空即用已存凭据」才成立。
//!
//! | 项 | pi（TS） | 这里 |
//! |---|---|---|
//! | 文件 | `<agentDir>/git-credentials.json` | `<toilerHome>/configs/git-credentials.json` |
//! | 结构 | `{version:1, entries:{<sha256(remote)>:{iv,tag,ciphertext}}}` | 同左，**逐字段兼容** |
//! | 算��� | AES-256-GCM，12 字节随机 IV，认证标签单独存 | 同左 |
//! | 明文 | `{kind, username?, secret}` 的 JSON | 同左 |
//! | 数据密钥 | macOS Keychain（service `pi-desktop-git-credentials` / account `encryption-key`） | **复用同一个条目**：pi 存过的凭据 toiler 能直接解开，反之亦然 |
//! | 其它平台 | 「安全存储不可用」，明确拒绝而不是退回可还原的本地密钥 | 同左 |
//!
//! 密钥放在 OS 钥匙串而不是文件里，是这套方案唯一的安全边界：文件本身可以
//! 随便复制（会话导出、备份、误提交），没有密钥就是一串密文。所以非 macOS
//! 平台宁可报「不可用」，也不自己生成本地密钥文件 —— 那只是把明文换成
//! 同一目录下的一把明文钥匙。
//!
//! 读失败（文件损坏 / 密钥不匹配 / 解密不出来）一律当「没有已存凭据」，
//! 绝不因为解不开就覆盖掉原文件：那份密文可能属于 pi，写坏了用户就去不回来了。

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, KeyInit};
use aes_gcm::{Aes256Gcm, Key, Nonce};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine as _;
use rand::RngCore;
use serde_json::{json, Map, Value};
use sha2::{Digest, Sha256};

/// 与 pi 同名同位置语义的文件（toiler 放在 `configs/` 下）。
const CREDENTIAL_FILE: &str = "git-credentials.json";
/// Keychain 条目名沿用 pi 的，见模块文档的兼容性说明。
const KEYCHAIN_SERVICE: &str = "pi-desktop-git-credentials";
const KEYCHAIN_ACCOUNT: &str = "encryption-key";
const KEY_BYTES: usize = 32;
const IV_BYTES: usize = 12;
const TAG_BYTES: usize = 16;

/// 一条 git 凭据：HTTPS 是「用户名 + 令牌/密码」，SSH 是「私钥口令」。
#[derive(Clone, PartialEq, Eq)]
pub struct GitCredential {
    /// `https` / `ssh`。
    pub kind: String,
    /// HTTPS 必填（pi 的解密校验同样要求 https 带字符串用户名）。
    pub username: Option<String>,
    pub secret: String,
}

// 手写 Debug：这个结构会出现在日志与测试失败信息里，secret 不能跟着出去。
impl std::fmt::Debug for GitCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitCredential")
            .field("kind", &self.kind)
            .field("username", &self.username)
            .field("secret", &"<hidden>")
            .finish()
    }
}

impl GitCredential {
    pub fn new(
        kind: impl Into<String>,
        username: Option<String>,
        secret: impl Into<String>,
    ) -> Self {
        Self {
            kind: kind.into(),
            username,
            secret: secret.into(),
        }
    }

    fn validate(&self) -> Result<(), String> {
        if self.kind != "https" && self.kind != "ssh" {
            return Err("Invalid Git credential".into());
        }
        if self.secret.is_empty() {
            return Err("Invalid Git credential".into());
        }
        if self.kind == "https" && self.username.as_deref().unwrap_or("").trim().is_empty() {
            return Err("A username is required for an HTTPS remote".into());
        }
        Ok(())
    }
}

/// 加密凭据文件句柄。
#[derive(Debug, Clone)]
pub struct CredentialStore {
    path: PathBuf,
    /// 显式密钥：只给测试用（不碰系统钥匙串）。生产路径永远是 `None`，
    /// 密钥只能从钥匙串来。
    key: Option<Vec<u8>>,
}

impl Default for CredentialStore {
    fn default() -> Self {
        Self::new(crate::paths::config_file(CREDENTIAL_FILE))
    }
}

impl CredentialStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            key: None,
        }
    }

    /// 用指定密钥的存储（测试用）。生产代码**不要**用它：密钥一旦能由调用方
    /// 传入，就有人会写成一个常量或写进配置文件。
    pub fn with_key(path: impl Into<PathBuf>, key: Vec<u8>) -> Self {
        Self {
            path: path.into(),
            key: Some(key),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn key(&self) -> Option<Vec<u8>> {
        self.key.clone().or_else(data_key)
    }

    /// 读一条已存凭据。`expected_kind` 与存的种类不符（远程换了协议 / 换了
    /// 仓库）就当没有 —— 否则会把私钥口令当 HTTPS 令牌送去认证。
    ///
    /// 安全存储不可用、文件坏掉、密钥对不上，全部返回 `None`。
    pub fn load(&self, remote: &str, expected_kind: &str) -> Option<GitCredential> {
        self.load_with_key(remote, expected_kind, &self.key()?)
    }

    /// 是否存过这个远程的凭据；有就返回用户名（**不是**密钥，用户名不是秘密，
    /// 面板要靠它把用户名框回填出来）。
    ///
    /// 外层 `Option` 是「存没存过」，内层是「用户名有没有值」（SSH 凭据没有）。
    pub fn saved_username(&self, remote: &str, expected_kind: &str) -> Option<Option<String>> {
        self.load(remote, expected_kind).map(|c| c.username)
    }

    /// 写入（或覆盖）一条凭据。整体重写整份文件：entries 是哈希 → 密文的映射，
    /// 没有「追加」这种语义。
    pub fn save(&self, remote: &str, credential: &GitCredential) -> Result<(), String> {
        credential.validate()?;
        let key = self
            .key()
            .ok_or_else(|| SECURE_STORAGE_UNAVAILABLE.to_string())?;
        self.save_with_key(remote, credential, &key)
    }

    fn load_with_key(
        &self,
        remote: &str,
        expected_kind: &str,
        key: &[u8],
    ) -> Option<GitCredential> {
        let entries = read_entries(&self.path)?;
        let entry = entries.get(&remote_key(remote))?;
        let credential = decrypt_entry(entry, key)?;
        if credential.kind != expected_kind {
            return None;
        }
        credential.validate().ok()?;
        Some(credential)
    }

    fn save_with_key(
        &self,
        remote: &str,
        credential: &GitCredential,
        key: &[u8],
    ) -> Result<(), String> {
        credential.validate()?;
        // 读不出来（坏文件 / 未知 version）就不写：宁可这次「记住」失败，
        // 也不要拿一份空 entries 把别人存的东西盖掉。
        let mut entries = read_entries(&self.path)
            .ok_or_else(|| "Existing Git credential store is invalid".to_string())?;
        entries.insert(remote_key(remote), encrypt_entry(credential, key)?);
        let body = json!({ "version": 1, "entries": Value::Object(entries) });
        let text = serde_json::to_string(&body).map_err(|e| e.to_string())?;
        write_private_file(&self.path, &text)
    }
}

const SECURE_STORAGE_UNAVAILABLE: &str =
    "Secure credential storage is unavailable on this platform";

/// `remote` → entries 的键：与 pi 完全一致（先去掉 URL 里的用户名与尾部斜杠，
/// 再取 sha256 的十六进制）。散列而不是原串，是为了不把带 token 的远程地址
/// 写进文件当索引。
pub fn remote_key(remote: &str) -> String {
    let normalized = normalize_remote(remote);
    let digest = Sha256::digest(normalized.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// pi 的 `remoteKey()` 里那两步字符串处理：
/// `https?://[^/@]+@` 前缀（内嵌用户名）去掉、尾部斜杠去掉。
pub fn normalize_remote(remote: &str) -> String {
    strip_userinfo(remote.trim())
        .trim_end_matches('/')
        .to_string()
}

fn strip_userinfo(url: &str) -> String {
    // 只看 `://` 之后到第一个 `/` 之前这一段（authority）里的 `@`。
    for scheme in ["https://", "http://"] {
        let lower = url.to_ascii_lowercase();
        let Some(rest) = lower.strip_prefix(scheme) else {
            continue;
        };
        let authority_end = rest.find('/').unwrap_or(rest.len());
        let Some(at) = rest[..authority_end].rfind('@') else {
            return url.to_string();
        };
        return format!("{}{}", &url[..scheme.len()], &url[scheme.len() + at + 1..]);
    }
    url.to_string()
}

/// 读 entries；文件不存在 = 空表，读不动或结构不认识 = `None`（调用方据此
/// 拒绝写入，见 [`CredentialStore::save_with_key`]）。
fn read_entries(path: &Path) -> Option<Map<String, Value>> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Some(Map::new()),
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "读取 Git 凭据文件失败");
            return None;
        }
    };
    let parsed: Value = match serde_json::from_str(&text) {
        Ok(value) => value,
        Err(error) => {
            tracing::warn!(path = %path.display(), %error, "Git 凭据文件不是合法 JSON");
            return None;
        }
    };
    if parsed.get("version").and_then(Value::as_u64) != Some(1) {
        return None;
    }
    parsed
        .get("entries")
        .and_then(Value::as_object)
        .cloned()
        .or_else(|| Some(Map::new()))
}

fn encrypt_entry(credential: &GitCredential, key: &[u8]) -> Result<Value, String> {
    let plaintext = serde_json::to_vec(&json!({
        "kind": credential.kind,
        "username": credential.username,
        "secret": credential.secret,
    }))
    .map_err(|e| e.to_string())?;
    let mut iv = [0u8; IV_BYTES];
    rand::thread_rng().fill_bytes(&mut iv);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    // `encrypt` 的输出是 密文 ‖ 16 字节认证标签，pi 把标签单独存一个字段，
    // 所以这里要拆开；解密时再拼回去。
    let sealed = cipher
        .encrypt(Nonce::from_slice(&iv), plaintext.as_slice())
        .map_err(|_| "Failed to encrypt Git credential".to_string())?;
    let (ciphertext, tag) = sealed.split_at(sealed.len().saturating_sub(TAG_BYTES));
    Ok(json!({
        "iv": BASE64.encode(iv),
        "tag": BASE64.encode(tag),
        "ciphertext": BASE64.encode(ciphertext),
    }))
}

fn decrypt_entry(value: &Value, key: &[u8]) -> Option<GitCredential> {
    let field = |name: &str| -> Option<Vec<u8>> { BASE64.decode(value.get(name)?.as_str()?).ok() };
    let iv = field("iv")?;
    let tag = field("tag")?;
    let ciphertext = field("ciphertext")?;
    if iv.len() != IV_BYTES || tag.len() != TAG_BYTES {
        return None;
    }
    let mut sealed = ciphertext;
    sealed.extend_from_slice(&tag);
    let cipher = Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key));
    let plaintext = cipher
        .decrypt(Nonce::from_slice(&iv), sealed.as_slice())
        .ok()?;
    let parsed: Value = serde_json::from_slice(&plaintext).ok()?;
    let kind = parsed.get("kind")?.as_str()?.to_string();
    let secret = parsed.get("secret")?.as_str()?.to_string();
    let username = match parsed.get("username") {
        None | Some(Value::Null) => None,
        Some(Value::String(text)) => Some(text.clone()),
        Some(_) => return None,
    };
    Some(GitCredential {
        kind,
        username,
        secret,
    })
}

/// 原子写 + `0600`（与 `auth.rs` 的写法一致：临时文件先给权限再 rename，
/// 避免出现一个「内容已就位但权限还没收紧」的窗口）。
fn write_private_file(path: &Path, body: &str) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // 目录 0700：凭据文件的**文件名**本身不敏感，但同目录还躺着 auth.json。
            let _ = std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700));
        }
    }
    let tmp = path.with_extension(format!("tmp-{}", std::process::id()));
    {
        use std::io::Write;
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .open(&tmp)
            .map_err(|e| e.to_string())?;
        crate::auth::set_mode_600(&tmp).map_err(|e| e.to_string())?;
        file.write_all(body.as_bytes()).map_err(|e| e.to_string())?;
        file.flush().map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    crate::auth::set_mode_600(path).map_err(|e| e.to_string())?;
    Ok(())
}

// ============================================================================
// 数据密钥（macOS 钥匙串）
// ============================================================================

/// 进程内缓存：成功拿到密钥后不再反复 spawn `security`（面板每次刷新状态都会
/// 读一次凭据）。失败不缓存 —— 钥匙串可能只是暂时锁着。
static DATA_KEY: std::sync::Mutex<Option<Vec<u8>>> = std::sync::Mutex::new(None);

/// 本平台能不能加密保存凭据。前端据此决定「记住」勾选框要不要置灰：
///
/// * macOS：`/usr/bin/security` 在就为真（钥匙串必然存在）；
/// * 其它平台：永远为假 —— 没有等价的系统密钥库，凭据无法安全落盘。
///   勾上也不会真的保存，让勾选框永远勾着但每次都要重输，比直接告诉用户更难受。
///
/// **这是只读探测**：不会顺手创建钥匙串条目。它每次 `GET /api/git` 都会跑，
/// 而「看一眼面板就在系统钥匙串里多一条记录」是不该有的副作用 —— 密钥的创建
/// 只发生在真正要存凭据的那一次。
///
/// 注意 HTTPS 仍可由 git 自己的 credential helper（`osxkeychain` / `manager` /
/// `store`）兜住，那条路由 git 负责，不归这里管。
pub fn secure_storage_available() -> bool {
    #[cfg(target_os = "macos")]
    {
        std::path::Path::new(SECURITY_CLI).is_file()
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// macOS 的钥匙串命令。写在本文件里就不用在每个 `Command::new` 处重复字面量。
#[cfg(target_os = "macos")]
const SECURITY_CLI: &str = "/usr/bin/security";

fn data_key() -> Option<Vec<u8>> {
    let mut cached = DATA_KEY.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(key) = cached.as_ref() {
        return Some(key.clone());
    }
    let key = keychain_key()?;
    *cached = Some(key.clone());
    Some(key)
}

/// 从系统钥匙串取数据密钥；没有就新建一把。
///
/// 条目名与 pi 相同（[`KEYCHAIN_SERVICE`]），这是刻意的：pi 存过的密文 toiler
/// 要能直接解开，两边的 `git-credentials.json` 才能互相搬。
#[cfg(target_os = "macos")]
fn keychain_key() -> Option<Vec<u8>> {
    use std::process::{Command, Stdio};

    let read_key = || -> Option<Vec<u8>> {
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-a",
                KEYCHAIN_ACCOUNT,
                "-s",
                KEYCHAIN_SERVICE,
                "-w",
            ])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let text = String::from_utf8_lossy(&output.stdout);
        let key = BASE64.decode(text.trim()).ok()?;
        (key.len() == KEY_BYTES).then_some(key)
    };

    if let Some(key) = read_key() {
        return Some(key);
    }
    if keychain_has_entry() {
        // 条目在但我解不出来（不是 32 字节 base64）。**不覆盖**：那份密钥可能
        // 还牵着 pi 的密文，替用户重写等于让旧凭据永久失效。
        tracing::warn!("Keychain 里的 Git 凭据密钥格式不对，本次不启用加密凭据");
        return None;
    }

    let mut key = vec![0u8; KEY_BYTES];
    rand::thread_rng().fill_bytes(&mut key);
    let encoded = BASE64.encode(&key);
    let status = Command::new("security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            KEYCHAIN_ACCOUNT,
            "-s",
            KEYCHAIN_SERVICE,
            "-w",
            &encoded,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .ok()?;
    if !status.success() {
        tracing::warn!("写入 Keychain 失败，本次不启用加密凭据");
        return None;
    }
    // 回读一遍再返回：`-U` 是「后写者胜」，并发时刚写进去的可能已被别人替换，
    // 而我们必须用文件里那份密文对应的密钥。
    read_key()
}

/// 钥匙串里是否已有条目（只看存在性，不需要 `-w` 就不会命中 ACL 授权）。
#[cfg(target_os = "macos")]
fn keychain_has_entry() -> bool {
    use std::process::{Command, Stdio};
    Command::new("security")
        .args([
            "find-generic-password",
            "-a",
            KEYCHAIN_ACCOUNT,
            "-s",
            KEYCHAIN_SERVICE,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

/// 非 macOS：没有等价的、不落明文的系统密钥库，明确报告「不可用」。
/// 面板据此退回「本次有效」的行为（HTTPS 仍可由 credential helper 兜住）。
#[cfg(not(target_os = "macos"))]
fn keychain_key() -> Option<Vec<u8>> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_KEY: [u8; KEY_BYTES] = [7u8; KEY_BYTES];

    /// 测试用存储：路径在临时目录，密钥直接给定（**不**碰系统钥匙串，
    /// 否则 CI 与没授权的机器上这些用例会成批失败）。
    fn temp_store(tag: &str) -> (CredentialStore, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "toiler-git-credentials-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(CREDENTIAL_FILE);
        let store = CredentialStore::with_key(path, TEST_KEY.to_vec());
        (store, dir)
    }

    /// 远程归一化：内嵌用户名与尾部斜杠都要抹掉，否则同一个仓库换种写法就
    /// 找不到已存凭据（面板显示「未保存」，口令被清空）。
    #[test]
    fn remote_key_ignores_userinfo_and_trailing_slash() {
        let a = remote_key("https://github.com/acme/repo.git");
        assert_eq!(a, remote_key("https://github.com/acme/repo.git/"));
        assert_eq!(a, remote_key("https://alice@github.com/acme/repo.git"));
        assert_eq!(
            a,
            remote_key("https://alice:token@github.com/acme/repo.git")
        );
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
        // 换一个仓库必须换一个键
        assert_ne!(a, remote_key("https://github.com/acme/other.git"));
    }

    /// 往返：存进去的密文要能解回一模一样的凭据，且文件里看不到明文。
    #[test]
    fn credential_round_trips_through_encryption() {
        let (store, dir) = temp_store("roundtrip");
        let saved = GitCredential::new("https", Some("alice".into()), "s3cr3t-token");
        store
            .save("https://github.com/acme/repo.git", &saved)
            .unwrap();

        let loaded = store
            .load("https://github.com/acme/repo.git", "https")
            .expect("刚存的凭据必须能读回来");
        assert_eq!(loaded, saved);
        assert_eq!(
            store.saved_username("https://github.com/acme/repo.git", "https"),
            Some(Some("alice".to_string()))
        );

        // 文件结构对齐 pi：version 1 + 三个 base64 字段，明文一个字节都不许有。
        let text = std::fs::read_to_string(store.path()).unwrap();
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["version"], 1);
        let entry = &parsed["entries"][remote_key("https://github.com/acme/repo.git")];
        for field in ["iv", "tag", "ciphertext"] {
            assert!(entry[field].as_str().is_some(), "缺字段 {field}: {text}");
        }
        assert!(!text.contains("s3cr3t-token"), "明文泄漏到文件里：{text}");
        assert!(!text.contains("alice"), "用户名也是明文：{text}");

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "凭据文件必须是 0600");
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 密钥不对 / 内容被改过时必须解不出来（AES-GCM 的认证标签），而不是
    /// 返回半截凭据去试认证。
    #[test]
    fn wrong_key_and_tampering_yield_no_credential() {
        let (store, dir) = temp_store("tamper");
        store
            .save(
                "git@github.com:acme/repo.git",
                &GitCredential::new("ssh", None, "passphrase"),
            )
            .unwrap();
        // 换一把密钥（直接读文件、用别的密钥解）必须解不出
        let other = CredentialStore::with_key(store.path(), vec![9u8; KEY_BYTES]);
        assert!(
            other.load("git@github.com:acme/repo.git", "ssh").is_none(),
            "换一把密钥必须解不出"
        );

        // 改一字节密文
        let text = std::fs::read_to_string(store.path()).unwrap();
        let mut parsed: Value = serde_json::from_str(&text).unwrap();
        let key = remote_key("git@github.com:acme/repo.git");
        let ciphertext = parsed["entries"][&key]["ciphertext"].as_str().unwrap();
        let mut bytes = BASE64.decode(ciphertext).unwrap();
        bytes[0] ^= 0xff;
        parsed["entries"][&key]["ciphertext"] = json!(BASE64.encode(bytes));
        std::fs::write(store.path(), serde_json::to_string(&parsed).unwrap()).unwrap();
        assert!(store.load("git@github.com:acme/repo.git", "ssh").is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 远程换了协议（https → ssh）时不能把旧口令当新协议的凭据送出去。
    #[test]
    fn kind_mismatch_is_not_reused() {
        let (store, dir) = temp_store("kind");
        store
            .save(
                "https://example.com/repo.git",
                &GitCredential::new("https", Some("alice".into()), "token"),
            )
            .unwrap();
        assert!(store.load("https://example.com/repo.git", "ssh").is_none());
        assert!(store
            .load("https://example.com/repo.git", "https")
            .is_some());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件坏掉时既不能崩，也不能被后续保存覆盖成空表（那会让用户已存的
    /// 凭据永久消失）。
    #[test]
    fn invalid_store_file_is_never_overwritten() {
        let (store, dir) = temp_store("broken");
        std::fs::write(store.path(), "{not json").unwrap();
        assert!(store
            .load("https://example.com/repo.git", "https")
            .is_none());
        let error = store
            .save(
                "https://example.com/repo.git",
                &GitCredential::new("https", Some("alice".into()), "token"),
            )
            .unwrap_err();
        assert!(error.contains("invalid"), "{error}");
        assert_eq!(std::fs::read_to_string(store.path()).unwrap(), "{not json");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// HTTPS 缺用户名在 pi 侧是不合法条目（解出来也会被丢），这里提前拒绝，
    /// 免得存进去一条永远读不回来的记录。
    #[test]
    fn https_requires_a_username() {
        let (store, dir) = temp_store("validate");
        let error = store
            .save(
                "https://example.com/repo.git",
                &GitCredential::new("https", None, "token"),
            )
            .unwrap_err();
        assert!(error.contains("username"), "{error}");
        let error = store
            .save(
                "https://example.com/repo.git",
                &GitCredential::new("https", Some("alice".into()), ""),
            )
            .unwrap_err();
        assert!(error.contains("Invalid"), "{error}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
