//! Pi 官网目前提供服务端渲染的目录页，没有公开的目录 JSON 契约。
//! 只提取卡片上的文本和数字，不将第三方 HTML 或脚本传给前端。

use std::time::Duration;

use anyhow::{anyhow, bail, ensure, Result};
use axum::{extract::Query, http::StatusCode, response::IntoResponse, Json};
use dom_query::Document;
use serde::{Deserialize, Serialize};
use serde_json::json;
use url::Url;

const CATALOG_URL: &str = "https://pi.dev/packages";
const SITE_PAGE_SIZE: usize = 50;
const MAX_RESPONSE_BYTES: usize = 2 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct CatalogQuery {
    q: Option<String>,
    from: Option<i64>,
    size: Option<i64>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Plugin {
    name: String,
    source: String,
    description: String,
    description_language: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    publisher: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    monthly_downloads: Option<u64>,
    npm_url: String,
    homepage_url: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct Catalog {
    plugins: Vec<Plugin>,
    from: usize,
    size: usize,
    has_more: bool,
}

struct SitePage {
    plugins: Vec<Plugin>,
    has_next: bool,
}

pub async fn catalog(Query(query): Query<CatalogQuery>) -> impl IntoResponse {
    let search = query.q.as_deref().unwrap_or_default().trim();
    if search.chars().count() > 200 {
        return (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "插件搜索词不能超过 200 个字符"})),
        )
            .into_response();
    }
    let from = usize::try_from(query.from.unwrap_or(0).max(0)).unwrap_or(usize::MAX);
    let size = query.size.unwrap_or(40).clamp(1, SITE_PAGE_SIZE as i64) as usize;
    match fetch_catalog(search, from, size).await {
        Ok(result) => Json(result).into_response(),
        Err(error) => {
            tracing::warn!(%error, "获取 Pi 官网插件目录失败");
            (
                StatusCode::BAD_GATEWAY,
                Json(json!({"error": "暂时无法加载 Pi 官网插件目录"})),
            )
                .into_response()
        }
    }
}

async fn fetch_catalog(search: &str, from: usize, size: usize) -> Result<Catalog> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()?;
    read_catalog(from, size, |page| fetch_page(&client, search, page)).await
}

async fn read_catalog<F, Fut>(from: usize, size: usize, mut fetch: F) -> Result<Catalog>
where
    F: FnMut(usize) -> Fut,
    Fut: std::future::Future<Output = Result<SitePage>>,
{
    let page = from / SITE_PAGE_SIZE + 1;
    let offset = from % SITE_PAGE_SIZE;
    let first = fetch(page).await?;
    let mut plugins = Vec::new();
    let mut has_more = append_page(&mut plugins, first, offset, size);
    // 兼容使用 40 条或其他页长的旧调用方；当前界面用 50 条，始终只取一页。
    if plugins.len() < size && has_more {
        has_more = append_page(&mut plugins, fetch(page + 1).await?, 0, size);
    }
    Ok(Catalog {
        plugins,
        from,
        size,
        has_more,
    })
}

fn append_page(out: &mut Vec<Plugin>, page: SitePage, offset: usize, size: usize) -> bool {
    let remaining = size - out.len();
    let has_more = page.plugins.len().saturating_sub(offset) > remaining || page.has_next;
    out.extend(page.plugins.into_iter().skip(offset).take(remaining));
    has_more
}

fn page_url(search: &str, page: usize) -> Url {
    let mut url = Url::parse(CATALOG_URL).expect("内置的 Pi 目录 URL 必须有效");
    if !search.is_empty() {
        url.query_pairs_mut().append_pair("name", search);
    }
    if page > 1 {
        url.query_pairs_mut().append_pair("page", &page.to_string());
    }
    url
}

async fn fetch_page(client: &reqwest::Client, search: &str, page: usize) -> Result<SitePage> {
    let mut response = client.get(page_url(search, page)).send().await?;
    ensure!(
        response.status().is_success(),
        "官网返回 HTTP {}",
        response.status()
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            body.len() + chunk.len() <= MAX_RESPONSE_BYTES,
            "官网目录响应过大"
        );
        body.extend_from_slice(&chunk);
    }
    parse_page(std::str::from_utf8(&body)?, page)
}

fn parse_page(body: &str, requested_page: usize) -> Result<SitePage> {
    let doc = Document::from(body);
    let grid = doc.select(".packages-grid");
    let empty = || SitePage {
        plugins: Vec::new(),
        has_next: false,
    };
    if grid.is_empty() {
        if !doc.select(".packages-empty").is_empty() {
            return Ok(empty());
        }
        bail!("官网目录响应中没有插件列表");
    }
    let pagination = doc.select(".packages-pagination");
    let current = if pagination.is_empty() {
        1
    } else {
        pagination
            .select("[aria-current=page]")
            .text()
            .trim()
            .parse::<usize>()?
    };
    // 官网会把越界请求钳到末页；不能把末页重复返回给前端。
    if current < requested_page {
        return Ok(empty());
    }
    ensure!(current == requested_page, "官网返回了意外的分页");
    let mut plugins = Vec::new();
    for card in grid
        .select("article[data-package-card][data-package-name]")
        .iter()
    {
        let raw_name = card
            .attr("data-package-name")
            .ok_or_else(|| anyhow!("官网插件缺少包名"))?;
        let name = raw_name.as_ref();
        // 将远程包名变成可安装的 npm spec 前，拒绝参数、路径、版本及 URL 等其他语法。
        ensure!(valid_package_name(name), "官网返回了无效的 npm 包名");
        let publisher = card
            .select(".packages-meta span:first-child")
            .text()
            .trim()
            .to_string();
        plugins.push(Plugin {
            name: name.to_string(),
            source: format!("npm:{name}"),
            description: card.select(".packages-desc").text().trim().to_string(),
            description_language: "und",
            publisher: (!publisher.is_empty()).then_some(publisher),
            monthly_downloads: card
                .attr("data-package-downloads")
                .and_then(|raw| raw.parse().ok()),
            npm_url: format!("https://www.npmjs.com/package/{name}"),
            homepage_url: format!("{CATALOG_URL}/{name}"),
        });
    }
    let has_next = !pagination.select("a.pagination-link:last-child").is_empty();
    ensure!(
        !plugins.is_empty()
            && plugins.len() <= SITE_PAGE_SIZE
            && (!has_next || plugins.len() == SITE_PAGE_SIZE),
        "官网目录的分页结构已变化"
    );
    Ok(SitePage { plugins, has_next })
}

fn valid_package_name(name: &str) -> bool {
    let valid_part = |part: &str| {
        !part.is_empty()
            && !part.starts_with(['.', '-', '_'])
            && part
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'_' | b'.' | b'~'))
    };
    name.len() <= 214
        && match name.strip_prefix('@') {
            Some(scoped) => scoped
                .split_once('/')
                .is_some_and(|(scope, package)| valid_part(scope) && valid_part(package)),
            None => valid_part(name),
        }
}

#[cfg(test)]
mod tests;
