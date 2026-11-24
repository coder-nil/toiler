//! skills.sh 市场适配：搜索公开目录，并把选中的 GitHub 技能安全落到本地。
//!
//! 安装默认先走 `gh-proxy.com`，再回退 GitHub 原地址。这样直连
//! `codeload.github.com` 被网络策略挂起时，不会等完直连超时才开始镜像请求。
//!
//! skills.sh 的公开站点搜索接口不需要 Vercel OIDC；官方 `/api/v1` 目录接口目前需要
//! 令牌，因此这里使用站点实际给浏览器调用的 `/api/search`。安装不执行 `npx skills`：
//! Toiler 运行时不依赖 Node，而是下载公开 GitHub 归档、只解出选中的技能目录。

use std::collections::HashMap;
use std::io::Read;
use std::path::{Component, Path};
use std::time::Duration;

use flate2::read::GzDecoder;
use futures::StreamExt;
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tar::Archive;

mod installed;

pub use installed::{check, install, metadata_for_skill_file, remove, update, Scope};

const SEARCH_URL: &str = "https://skills.sh/api/search";
const SEARCH_LIMIT: u32 = 50;
const SEARCH_TIMEOUT: Duration = Duration::from_secs(20);
const ARCHIVE_TIMEOUT: Duration = Duration::from_secs(90);
const DEFAULT_GITHUB_MIRROR: &str = "https://gh-proxy.com/";
const MAX_QUERY_BYTES: usize = 200;
const MAX_SEARCH_RESPONSE_BYTES: usize = 2 * 1024 * 1024;
const MAX_ARCHIVE_BYTES: usize = 64 * 1024 * 1024;
const MAX_EXTRACTED_BYTES: usize = 128 * 1024 * 1024;
const MAX_FILES: usize = 2_000;
pub const INSTALL_METADATA_FILE: &str = ".toiler-skill.json";

/// 前端 `SkillSearchResult` 需要的旧 pi 形状。
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SearchResult {
    pub package: String,
    pub installs: String,
    pub url: String,
}

/// 安装后放在技能目录中的来源信息。它不是技能资源，不会被 rpi 加载。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallMetadata {
    pub package: String,
    pub scope: String,
    pub source: String,
    #[serde(rename = "sourceType")]
    pub source_type: String,
    #[serde(rename = "skillsShUrl")]
    pub skills_sh_url: String,
    #[serde(rename = "skillPath")]
    pub skill_path: String,
    #[serde(rename = "versionHash")]
    pub version_hash: String,
    #[serde(rename = "canCheckForUpdates")]
    pub can_check_for_updates: bool,
    #[serde(
        rename = "archivePath",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    archive_path: Option<String>,
}

#[derive(Debug)]
pub struct MarketError {
    pub status: StatusCode,
    pub message: String,
}

impl MarketError {
    fn bad_request(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.into(),
        }
    }

    fn upstream(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::BAD_GATEWAY,
            message: message.into(),
        }
    }

    fn timeout(upstream: &str) -> Self {
        Self {
            status: StatusCode::GATEWAY_TIMEOUT,
            message: format!("{upstream} 请求超时，请稍后重试"),
        }
    }
}

#[derive(Debug)]
enum ResponseReadError {
    TooLarge,
    Transport(reqwest::Error),
}

impl ResponseReadError {
    fn into_market_error(self, upstream: &str) -> MarketError {
        match self {
            Self::TooLarge => MarketError::upstream(format!("{upstream} 返回内容超过大小限制")),
            Self::Transport(error) if error.is_timeout() => MarketError::timeout(upstream),
            Self::Transport(error) => {
                tracing::warn!(upstream, error = ?error.without_url(), "读取市场响应失败");
                MarketError::upstream(format!("读取 {upstream} 响应失败，请稍后重试"))
            }
        }
    }
}

#[derive(Debug, Deserialize)]
struct SearchEnvelope {
    #[serde(default)]
    skills: Vec<RemoteSkill>,
}

#[derive(Debug, Deserialize)]
struct RemoteSkill {
    id: Option<String>,
    source: Option<String>,
    #[serde(rename = "skillId", alias = "skill_id", alias = "slug")]
    skill_id: Option<String>,
    installs: Option<Value>,
    url: Option<String>,
}

#[derive(Debug, Clone)]
struct GithubPackage {
    source: String,
    owner: String,
    repo: String,
    skill: Option<String>,
}

type ArchiveFiles = HashMap<String, Vec<u8>>;

#[derive(Debug)]
struct SkillBundle<'a> {
    directory: String,
    archive_path: String,
    files: Vec<(&'a str, &'a [u8])>,
    version_hash: String,
}

/// 调用 skills.sh 的公开站点搜索接口。
pub async fn search(query: &str) -> Result<Vec<SearchResult>, MarketError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(MarketError::bad_request("搜索关键词不能为空"));
    }
    if query.len() > MAX_QUERY_BYTES {
        return Err(MarketError::bad_request("搜索关键词过长"));
    }

    let client = reqwest::Client::builder()
        .user_agent("Toiler/skills-market")
        .build()
        .map_err(|_| MarketError::upstream("无法创建 skills.sh 请求"))?;
    tokio::time::timeout(
        SEARCH_TIMEOUT,
        search_with_client(&client, SEARCH_URL, query),
    )
    .await
    .map_err(|_| MarketError::timeout("skills.sh"))?
}

async fn search_with_client(
    client: &reqwest::Client,
    url: &str,
    query: &str,
) -> Result<Vec<SearchResult>, MarketError> {
    let limit = SEARCH_LIMIT.to_string();
    let mut can_retry = true;
    let body = loop {
        let response = client
            .get(url)
            .query(&[("q", query), ("limit", limit.as_str())])
            .send()
            .await
            .map_err(|error| {
                if error.is_timeout() {
                    MarketError::timeout("skills.sh")
                } else {
                    MarketError::upstream("skills.sh 搜索失败，请稍后重试")
                }
            })?;
        let status = response.status();
        if !status.is_success() {
            return Err(MarketError::upstream(format!(
                "skills.sh 搜索返回 HTTP {}",
                status.as_u16()
            )));
        }
        match read_limited(response, MAX_SEARCH_RESPONSE_BYTES).await {
            Ok(body) => break body,
            // 上游收到请求后仍可能提前关闭响应流。只读搜索可以从头重试一次，
            // 两次共用总时限；HTTP 错误、超时、超限和无效 JSON 都不重试。
            Err(ResponseReadError::Transport(error)) if can_retry && !error.is_timeout() => {
                tracing::debug!(error = ?error.without_url(), "skills.sh 响应中断，重试一次");
                can_retry = false;
            }
            Err(error) => return Err(error.into_market_error("skills.sh")),
        }
    };
    let envelope: SearchEnvelope = serde_json::from_slice(&body)
        .map_err(|_| MarketError::upstream("skills.sh 返回了无法识别的数据"))?;

    Ok(envelope
        .skills
        .into_iter()
        .filter_map(search_result)
        .collect())
}

async fn fetch_archive(package: GithubPackage) -> Result<ArchiveFiles, MarketError> {
    let archive_url = format!(
        "https://codeload.github.com/{}/{}/tar.gz/HEAD",
        package.owner, package.repo
    );
    let client = reqwest::Client::builder()
        .timeout(ARCHIVE_TIMEOUT)
        .user_agent("Toiler/skills-market")
        .build()
        .map_err(|_| MarketError::upstream("无法创建 GitHub 请求"))?;
    let body = download_archive(&client, &archive_url).await?;
    tokio::task::spawn_blocking(move || unpack_archive(&body))
        .await
        .map_err(|_| MarketError::upstream("读取 GitHub 技能归档失败"))?
}

async fn download_archive(
    client: &reqwest::Client,
    archive_url: &str,
) -> Result<Vec<u8>, MarketError> {
    let mut attempts: Vec<(String, bool)> = Vec::new();
    if let Some(mirror_url) = github_mirror_url(archive_url) {
        // 镜像优先：某些网络环境会让 codeload.github.com 一直挂起，
        // 先访问 gh-proxy，避免用户等完整的 GitHub 超时后才看到回退。
        attempts.push((mirror_url, true));
    }
    attempts.push((archive_url.to_string(), false));

    let mut last_error: Option<MarketError> = None;
    for (index, (url, is_mirror)) in attempts.iter().enumerate() {
        let is_mirror = *is_mirror;
        match client.get(url).send().await {
            Ok(response) => {
                let status = response.status();
                if !status.is_success() {
                    let retryable = status == StatusCode::FORBIDDEN
                        || status == StatusCode::TOO_MANY_REQUESTS
                        || status.is_server_error();
                    let error = MarketError::upstream(format!(
                        "{} 仓库下载返回 HTTP {}",
                        if is_mirror { "gh-proxy" } else { "GitHub" },
                        status.as_u16()
                    ));
                    if retryable && index + 1 < attempts.len() {
                        tracing::warn!(status = status.as_u16(), "技能归档请求失败，切换下载通道");
                        last_error = Some(error);
                        continue;
                    }
                    return Err(archive_error(error, attempts.len()));
                }
                match read_limited(response, MAX_ARCHIVE_BYTES).await {
                    Ok(body) => return Ok(body),
                    Err(ResponseReadError::TooLarge) => {
                        return Err(MarketError::upstream("GitHub 技能归档超过大小限制"));
                    }
                    Err(ResponseReadError::Transport(error)) => {
                        let market_error = ResponseReadError::Transport(error)
                            .into_market_error(if is_mirror { "gh-proxy" } else { "GitHub" });
                        if index + 1 < attempts.len() {
                            tracing::warn!("技能归档读取失败，切换下载通道");
                            last_error = Some(market_error);
                            continue;
                        }
                        return Err(archive_error(market_error, attempts.len()));
                    }
                }
            }
            Err(error) => {
                let market_error = if error.is_timeout() {
                    MarketError::timeout(if is_mirror { "gh-proxy" } else { "GitHub" })
                } else {
                    tracing::warn!(error = ?error.without_url(), "GitHub 归档请求失败");
                    MarketError::upstream(format!(
                        "{} 请求失败，请稍后重试",
                        if is_mirror { "gh-proxy" } else { "GitHub" }
                    ))
                };
                if index + 1 < attempts.len() {
                    tracing::warn!("技能归档请求失败，切换下载通道");
                    last_error = Some(market_error);
                    continue;
                }
                return Err(archive_error(market_error, attempts.len()));
            }
        }
    }
    Err(last_error.unwrap_or_else(|| MarketError::upstream("下载 GitHub 技能失败，请稍后重试")))
}

fn archive_error(error: MarketError, attempt_count: usize) -> MarketError {
    if attempt_count > 1 {
        MarketError {
            status: error.status,
            message: format!("gh-proxy 与 GitHub 下载均失败：{}", error.message),
        }
    } else {
        error
    }
}

fn github_mirror_url(archive_url: &str) -> Option<String> {
    let prefix = std::env::var("TOILER_GITHUB_MIRROR")
        .or_else(|_| std::env::var("TOILER_GH_PROXY"))
        .unwrap_or_else(|_| DEFAULT_GITHUB_MIRROR.to_string());
    mirror_url_from_prefix(&prefix, archive_url)
}

fn mirror_url_from_prefix(prefix: &str, target: &str) -> Option<String> {
    let prefix = prefix.trim();
    if prefix.is_empty()
        || prefix.eq_ignore_ascii_case("off")
        || prefix.eq_ignore_ascii_case("none")
    {
        return None;
    }
    let prefix = prefix.trim_end_matches('/');
    (!prefix.is_empty()).then(|| format!("{prefix}/{target}"))
}
fn search_result(remote: RemoteSkill) -> Option<SearchResult> {
    let id = remote.id.or_else(|| {
        Some(format!(
            "{}/{}",
            remote.source.as_deref()?,
            remote.skill_id.as_deref()?
        ))
    })?;
    let (source, skill) = split_skill_id(&id)?;
    let installs = remote
        .installs
        .as_ref()
        .and_then(Value::as_u64)
        .map(|n| format!("{} installs", format_number(n)))
        .unwrap_or_else(|| "— installs".into());
    Some(SearchResult {
        package: format!("{}@{}", source, skill),
        installs,
        url: remote
            .url
            .unwrap_or_else(|| format!("https://skills.sh/{id}")),
    })
}

fn split_skill_id(id: &str) -> Option<(String, String)> {
    let mut parts = id.split('/');
    let owner = parts.next()?;
    let repo = parts.next()?;
    let skill = parts.next()?;
    if parts.next().is_some() || owner.is_empty() || repo.is_empty() || skill.is_empty() {
        return None;
    }
    let source = format!("{owner}/{repo}");
    validate_source(&source).ok()?;
    validate_segment(skill).ok()?;
    Some((source, skill.into()))
}

fn parse_package(raw: &str) -> Result<GithubPackage, MarketError> {
    let raw = raw.trim();
    if raw.is_empty() || raw.len() > 300 {
        return Err(MarketError::bad_request("技能包标识无效"));
    }
    let (source, skill): (String, Option<&str>) =
        if let Some((source, skill)) = raw.rsplit_once('@') {
            (source.to_string(), Some(skill))
        } else {
            let parts: Vec<&str> = raw.split('/').collect();
            if parts.len() == 3 {
                (format!("{}/{}", parts[0], parts[1]), Some(parts[2]))
            } else {
                (raw.to_string(), None)
            }
        };
    validate_source(&source).map_err(|_| {
        MarketError::bad_request(
            "目前只支持从 skills.sh 安装 GitHub 技能（格式：owner/repo@skill）",
        )
    })?;
    let mut source_parts = source.split('/');
    let owner = source_parts.next().unwrap().to_string();
    let repo = source_parts.next().unwrap().to_string();
    let skill = skill
        .map(|name| {
            validate_segment(name).map_err(|_| {
                MarketError::bad_request("技能名称只能包含字母、数字、点、下划线和连字符")
            })?;
            Ok(name.to_string())
        })
        .transpose()?;
    Ok(GithubPackage {
        source: source.to_string(),
        owner,
        repo,
        skill,
    })
}

fn validate_source(source: &str) -> Result<(), ()> {
    let mut parts = source.split('/');
    let owner = parts.next().ok_or(())?;
    let repo = parts.next().ok_or(())?;
    if parts.next().is_some() || owner.is_empty() || repo.is_empty() {
        return Err(());
    }
    validate_segment(owner).and(validate_segment(repo))
}

fn validate_segment(value: &str) -> Result<(), ()> {
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    // Windows 会把尾点和设备名映射到别的目标，即便相对路径不包含 `..`。
    if value.is_empty()
        || value.ends_with('.')
        || matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit())
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    {
        return Err(());
    }
    Ok(())
}

fn unpack_archive(archive_bytes: &[u8]) -> Result<ArchiveFiles, MarketError> {
    let decoder = GzDecoder::new(archive_bytes);
    let mut archive = Archive::new(decoder);
    let mut files = HashMap::<String, Vec<u8>>::new();
    let mut total = 0usize;
    let entries = archive
        .entries()
        .map_err(|_| MarketError::upstream("GitHub 技能归档格式无效"))?;
    let mut seen = std::collections::HashSet::new();
    let mut wrapper = None;
    for (index, entry) in entries.enumerate() {
        if index >= MAX_FILES {
            return Err(MarketError::upstream("技能归档条目数量超过限制"));
        }
        let mut entry = entry.map_err(|_| MarketError::upstream("读取 GitHub 技能归档失败"))?;
        let entry_size = entry.size();
        if entry_size > (MAX_EXTRACTED_BYTES - total) as u64 {
            return Err(MarketError::upstream("技能归档解压后超过大小限制"));
        }
        total += entry_size as usize;
        if !entry.header().entry_type().is_file() {
            continue;
        }
        let full_path = safe_archive_path(
            entry
                .path()
                .map_err(|_| MarketError::upstream("技能归档包含无效路径"))?
                .as_ref(),
        )?;
        let (prefix, path) = full_path.split_once('/').expect("归档路径至少两段");
        if *wrapper.get_or_insert_with(|| prefix.to_owned()) != prefix
            || !seen.insert(path.to_ascii_lowercase())
        {
            return Err(MarketError::upstream("技能归档包含重复或不一致的路径"));
        }
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|_| MarketError::upstream("读取技能文件失败"))?;
        if data.len() as u64 != entry_size {
            return Err(MarketError::upstream("GitHub 技能归档不完整"));
        }
        files.insert(path.to_owned(), data);
    }
    Ok(files)
}

fn select_skill<'a>(
    files: &'a ArchiveFiles,
    package: &GithubPackage,
    pinned: Option<&str>,
    legacy_update: bool,
) -> Result<SkillBundle<'a>, MarketError> {
    let requested = package.skill.as_deref();
    let mut skill_files: Vec<&str> = files
        .keys()
        .map(String::as_str)
        .filter(|path| path.ends_with("/SKILL.md") || *path == "SKILL.md")
        .collect();
    skill_files.sort_unstable_by(|a, b| {
        a.matches('/')
            .count()
            .cmp(&b.matches('/').count())
            .then(a.cmp(b))
    });
    let chosen = if let Some(pinned) = pinned {
        // 技能被移除时不能把仓库内剩余的另一个技能装到原目录。
        skill_files.iter().copied().find(|path| *path == pinned)
    } else if let Some(requested) = requested {
        skill_files
            .iter()
            .copied()
            .find(|path| skill_directory(path) == Some(requested))
            .or_else(|| {
                let [only] = skill_files.as_slice() else {
                    return None;
                };
                (!legacy_update || *only == "SKILL.md").then_some(*only)
            })
    } else if skill_files.len() == 1 {
        skill_files.first().copied()
    } else {
        None
    }
    .ok_or_else(|| {
        if requested.is_some() || pinned.is_some() {
            MarketError::bad_request("GitHub 仓库中没有找到指定技能")
        } else {
            MarketError::bad_request("GitHub 仓库中没有唯一技能，请指定 owner/repo@skill")
        }
    })?;

    let prefix = chosen
        .rsplit_once('/')
        .map(|(parent, _)| format!("{parent}/"));
    let mut selected = Vec::new();
    for (path, data) in files {
        let relative = match &prefix {
            None => path.as_str(),
            Some(prefix) => match path.strip_prefix(prefix) {
                Some(relative) => relative,
                None => continue,
            },
        };
        if relative
            .split('/')
            .next()
            .is_some_and(|name| name.eq_ignore_ascii_case(INSTALL_METADATA_FILE))
        {
            continue;
        }
        selected.push((relative, data.as_slice()));
    }
    selected.sort_unstable_by_key(|(path, _)| *path);

    // 保留既有哈希算法；旧记录的版本无需重新安装也可以比较。
    let mut hasher = Sha256::new();
    for (path, data) in &selected {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(data);
    }
    let version_hash = format!("{:x}", hasher.finalize());
    Ok(SkillBundle {
        directory: skill_directory(chosen).unwrap_or(&package.repo).to_owned(),
        archive_path: chosen.to_owned(),
        files: selected,
        version_hash,
    })
}

fn safe_archive_path(path: &Path) -> Result<String, MarketError> {
    let mut parts = Vec::new();
    for component in path.components() {
        let Component::Normal(value) = component else {
            return Err(MarketError::upstream("技能归档包含越界路径"));
        };
        let value = value
            .to_str()
            .ok_or_else(|| MarketError::upstream("技能归档包含非 UTF-8 路径"))?;
        validate_segment(value).map_err(|_| MarketError::upstream("技能归档包含无效路径"))?;
        parts.push(value);
    }
    if parts.len() < 2 {
        return Err(MarketError::upstream("技能归档缺少仓库目录"));
    }
    Ok(parts.join("/"))
}

fn skill_directory(path: &str) -> Option<&str> {
    let path = path.strip_suffix("/SKILL.md")?;
    path.rsplit('/').next()
}

fn write_bundle(
    temp: &Path,
    files: &[(&str, &[u8])],
    metadata: &InstallMetadata,
) -> Result<(), ()> {
    for (relative, data) in files {
        let path = temp.join(relative);
        let parent = path.parent().ok_or(())?;
        std::fs::create_dir_all(parent).map_err(|_| ())?;
        std::fs::write(path, data).map_err(|_| ())?;
    }
    std::fs::write(
        temp.join(INSTALL_METADATA_FILE),
        serde_json::to_vec_pretty(metadata).map_err(|_| ())?,
    )
    .map_err(|_| ())
}

async fn read_limited(
    response: reqwest::Response,
    limit: usize,
) -> Result<Vec<u8>, ResponseReadError> {
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(ResponseReadError::TooLarge);
    }
    let mut stream = response.bytes_stream();
    let mut body = Vec::new();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(ResponseReadError::Transport)?;
        if body.len().saturating_add(chunk.len()) > limit {
            return Err(ResponseReadError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

fn format_number(number: u64) -> String {
    let text = number.to_string();
    let first = text.len() % 3;
    let mut output = String::new();
    if first != 0 {
        output.push_str(&text[..first]);
    }
    for chunk in text.as_bytes()[first..].chunks(3) {
        if !output.is_empty() {
            output.push(',');
        }
        output.push_str(std::str::from_utf8(chunk).unwrap_or_default());
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    const TRUNCATED_SEARCH: &str =
        "HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n4\r\n{";

    // 用回环 TCP 模拟上游提前断开；完整的 Axum 响应无法覆盖这条读取错误路径。
    async fn fixture_search(replies: &[&str]) -> Result<Vec<SearchResult>, MarketError> {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/api/search", listener.local_addr().unwrap());
        let serve = async {
            for reply in replies {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.ends_with(b"\r\n\r\n") {
                    request.push(stream.read_u8().await.unwrap());
                }
                assert!(request.starts_with(b"GET /api/search?q=react&limit=50 HTTP/1.1\r\n"));
                stream.write_all(reply.as_bytes()).await.unwrap();
                stream.shutdown().await.unwrap();
            }
            drop(listener);
        };
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(search_with_client(&client, &url, "react"), serve)
        })
        .await
        .expect("搜索应按预期次数请求模拟上游");
        result
    }

    #[tokio::test]
    async fn search_retries_a_truncated_body() {
        let body = r#"{"skills":[{"id":"owner/repo/react","installs":1234}]}"#;
        let ok = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let results = fixture_search(&[TRUNCATED_SEARCH, &ok]).await.unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].package, "owner/repo@react");
        assert_eq!(results[0].installs, "1,234 installs");
    }

    #[tokio::test]
    async fn search_reports_a_persistent_read_failure_after_one_retry() {
        let error = fixture_search(&[TRUNCATED_SEARCH, TRUNCATED_SEARCH])
            .await
            .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.message, "读取 skills.sh 响应失败，请稍后重试");
    }

    #[tokio::test]
    async fn search_keeps_http_error_even_if_its_body_is_incomplete() {
        let error = fixture_search(&[
            "HTTP/1.1 429 Too Many Requests\r\nContent-Length: 100\r\nConnection: close\r\n\r\n",
        ])
        .await
        .unwrap_err();
        assert_eq!(error.status, StatusCode::BAD_GATEWAY);
        assert_eq!(error.message, "skills.sh 搜索返回 HTTP 429");
    }

    #[tokio::test]
    #[ignore = "需要连接 skills.sh；用于确认真实搜索接口返回可安装的结果"]
    async fn live_skills_sh_search() {
        let results = search("react").await.unwrap();
        assert!(!results.is_empty());
        assert!(results
            .iter()
            .all(|result| parse_package(&result.package).is_ok()));
    }

    #[test]
    fn search_result_maps_current_skills_sh_shape() {
        let result = search_result(RemoteSkill {
            id: Some("vercel-labs/agent-skills/vercel-react-best-practices".into()),
            source: Some("vercel-labs/agent-skills".into()),
            skill_id: Some("vercel-react-best-practices".into()),
            installs: Some(json!(1234567)),
            url: None,
        })
        .unwrap();
        assert_eq!(
            result.package,
            "vercel-labs/agent-skills@vercel-react-best-practices"
        );
        assert_eq!(result.installs, "1,234,567 installs");
        assert_eq!(
            result.url,
            "https://skills.sh/vercel-labs/agent-skills/vercel-react-best-practices"
        );
    }

    #[test]
    fn package_parser_rejects_urls_and_path_traversal() {
        assert!(parse_package("owner/repo@skill").is_ok());
        assert!(parse_package("owner/repo/skill").is_ok());
        assert!(parse_package("https://github.com/owner/repo@skill").is_err());
        assert!(parse_package("owner/../repo@skill").is_err());
        assert!(parse_package("owner/repo@../skill").is_err());
    }

    #[test]
    fn archive_path_rejects_parent_components() {
        assert!(safe_archive_path(Path::new("repo/SKILL.md")).is_ok());
        assert!(safe_archive_path(Path::new("repo/../SKILL.md")).is_err());
        assert!(safe_archive_path(Path::new("/repo/SKILL.md")).is_err());
    }

    #[test]
    fn mirror_url_can_be_disabled_or_overridden() {
        assert_eq!(
            mirror_url_from_prefix(
                "https://gh-proxy.com/",
                "https://codeload.github.com/a/b/tar.gz/HEAD"
            ),
            Some("https://gh-proxy.com/https://codeload.github.com/a/b/tar.gz/HEAD".into())
        );
        assert_eq!(mirror_url_from_prefix("off", "https://example.com/a"), None);
        assert_eq!(mirror_url_from_prefix("", "https://example.com/a"), None);
    }

    #[test]
    fn number_formatting_is_stable() {
        assert_eq!(format_number(0), "0");
        assert_eq!(format_number(999), "999");
        assert_eq!(format_number(1000), "1,000");
        assert_eq!(format_number(123456789), "123,456,789");
    }
}
