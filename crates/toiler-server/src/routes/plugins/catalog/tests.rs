use super::*;

const CARD: &str = r#"
<article data-package-card="true" data-package-name="@fixture/pi-addon" data-package-downloads="12034">
  <p class="packages-desc">Words &amp; 中文 &lt;em&gt; <b>text</b></p>
  <div class="packages-meta"><span>maintainer</span><span>12K/mo</span></div>
</article>
"#;

#[test]
fn parses_plain_text_metrics_and_trusted_links() {
    let html = format!(r#"<div class="packages-grid">{CARD}</div>"#);
    let page = parse_page(&html, 1).unwrap();
    assert!(!page.has_next);
    let plugin = serde_json::to_value(&page.plugins[0]).unwrap();
    assert_eq!(plugin["source"], "npm:@fixture/pi-addon");
    assert_eq!(plugin["description"], "Words & 中文 <em> text");
    assert_eq!(plugin["publisher"], "maintainer");
    assert_eq!(plugin["monthlyDownloads"], 12034);
    assert_eq!(
        plugin["npmUrl"],
        "https://www.npmjs.com/package/@fixture/pi-addon"
    );
    assert_eq!(
        plugin["homepageUrl"],
        "https://pi.dev/packages/@fixture/pi-addon"
    );
}

#[test]
fn distinguishes_empty_results_from_bad_upstream_data() {
    let empty = parse_page(r#"<p class="packages-empty">No packages</p>"#, 1).unwrap();
    assert!(empty.plugins.is_empty() && !empty.has_next);
    assert!(parse_page("<h1>Maintenance</h1>", 1).is_err());
    assert!(parse_page(r#"<div class="packages-grid"></div>"#, 1).is_err());
    let malformed = format!(
        r#"<div class="packages-grid">{}</div>"#,
        CARD.replace("@fixture/pi-addon", "--prefix")
    );
    assert!(parse_page(&malformed, 1).is_err());
    for name in [
        "../child",
        "@fixture/../child",
        "name@latest",
        "https://other.test/name",
        "name%2fchild",
    ] {
        assert!(!valid_package_name(name));
    }
}

fn fixture_page(page: usize) -> Result<SitePage> {
    let count = if page == 1 { SITE_PAGE_SIZE } else { 3 };
    let current = page.min(2);
    let first = (current - 1) * SITE_PAGE_SIZE;
    let cards: String = (first..first + count)
        .map(|index| CARD.replace("@fixture/pi-addon", &format!("pi-addon-{index}")))
        .collect();
    let next = if page == 1 {
        r#"<a class="pagination-link" href="/packages?page=2">Next</a>"#
    } else {
        r#"<span class="pagination-link is-disabled">Next</span>"#
    };
    parse_page(
        &format!(
            r#"<div class="packages-grid">{cards}</div><nav class="packages-pagination"><a class="pagination-link" href="/packages">Previous</a><div><span aria-current="page">{current}</span></div>{next}</nav>"#
        ),
        page,
    )
}

#[tokio::test]
async fn slices_across_site_pages_and_does_not_repeat_a_clamped_last_page() {
    let fetch = |page| std::future::ready(fixture_page(page));
    let first = read_catalog(0, 50, fetch).await.unwrap();
    assert_eq!(first.plugins.len(), 50);
    assert!(first.has_more);
    let last = read_catalog(49, 40, fetch).await.unwrap();
    assert_eq!(
        last.plugins
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        ["pi-addon-49", "pi-addon-50", "pi-addon-51", "pi-addon-52"]
    );
    assert!(!last.has_more);
    let after_last = read_catalog(100, 50, fetch).await.unwrap();
    assert!(after_last.plugins.is_empty() && !after_last.has_more);
}

#[test]
fn only_sends_the_encoded_search_and_page_to_pi() {
    assert_eq!(page_url("", 1).as_str(), CATALOG_URL);
    let url = page_url("a&name=other#中文", 2);
    assert_eq!(url.host_str(), Some("pi.dev"));
    assert_eq!(url.fragment(), None);
    assert_eq!(
        url.query_pairs().collect::<Vec<_>>(),
        [
            ("name".into(), "a&name=other#中文".into()),
            ("page".into(), "2".into())
        ]
    );
}

#[tokio::test]
#[ignore = "需要连接 pi.dev；适用于官网分页和卡片结构走查"]
async fn live_pi_catalog_browse_search_and_empty() {
    let first = fetch_catalog("", 0, 50).await.unwrap();
    assert_eq!(first.plugins.len(), 50);
    assert!(first.has_more);
    let next = fetch_catalog("", 50, 50).await.unwrap();
    assert!(!next.plugins.is_empty());
    assert_ne!(first.plugins[0].name, next.plugins[0].name);
    let search = fetch_catalog("pi-mcp-adapter", 0, 50).await.unwrap();
    assert!(search
        .plugins
        .iter()
        .any(|entry| entry.name == "pi-mcp-adapter"));
    let empty = fetch_catalog("toiler-unmatched-fixture-844f555071", 0, 50)
        .await
        .unwrap();
    assert!(empty.plugins.is_empty() && !empty.has_more);
}
