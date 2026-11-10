//! Brave Search (web) provider.
//!
//! Brave runs its own index and answers every query from it. It is a search
//! engine only: there is no page-fetch or site-map endpoint, so the service
//! never routes `web_fetch` / `web_map` here (`ProviderSpec::supports_fetch`
//! and `supports_map` are both false).
//!
//! Filters: recency maps to Brave's `freshness` parameter (shortcut values
//! when the window matches one exactly, otherwise an explicit date range).
//! Domain include/exclude use the documented `site:` / `-site:` query
//! operators, and results are post-filtered by host so the filter contract
//! holds even when the operator is not honored (a multi-domain include list is
//! enforced by the post-filter alone).

use std::time::Duration;

use reqwest::Client;
use serde_json::Value;

use crate::clock::{now_unix_seconds, utc_date};
use crate::error::Result;
use crate::model::search::SearchFilters;
use crate::model::source::Source;

use super::http::{build_client, get_json_with_header_auth};

const AUTH_HEADER: &str = "X-Subscription-Token";

/// Brave's documented `count` ceiling for one web search page.
const MAX_COUNT: usize = 20;

#[derive(Clone)]
pub struct BraveProvider {
    client: Client,
    api_url: String,
    api_key: String,
}

impl BraveProvider {
    pub fn new(api_url: impl Into<String>, api_key: impl Into<String>, timeout: Duration) -> Self {
        Self::with_client(build_client(timeout), api_url, api_key)
    }

    /// Construct with an externally provided `reqwest::Client`, shared with
    /// the other providers.
    pub fn with_client(
        client: Client,
        api_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            client,
            api_url: api_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
        }
    }

    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<Source>> {
        let params = brave_search_params(query, max_results, filters, now_unix_seconds());
        let raw = get_json_with_header_auth(
            &self.client,
            &format!("{}/res/v1/web/search", self.api_url),
            &params,
            (AUTH_HEADER, &self.api_key),
            "Brave",
        )
        .await?;
        let mut sources = enforce_domain_filters(normalize_brave_results(&raw), filters);
        sources.truncate(max_results.max(1));
        Ok(sources)
    }
}

/// Query parameters for `GET /res/v1/web/search`.
pub fn brave_search_params(
    query: &str,
    max_results: usize,
    filters: &SearchFilters,
    now_unix: u64,
) -> Vec<(&'static str, String)> {
    let mut q = query.trim().to_string();
    let includes: Vec<String> = filters
        .include_domains
        .iter()
        .filter_map(|domain| normalize_domain(domain))
        .collect();
    // One include domain maps cleanly onto `site:`. Several would need an OR
    // operator Brave does not document, so those rely on the post-filter.
    if let [only] = includes.as_slice() {
        q.push_str(&format!(" site:{only}"));
    }
    for domain in filters
        .exclude_domains
        .iter()
        .filter_map(|domain| normalize_domain(domain))
    {
        q.push_str(&format!(" -site:{domain}"));
    }

    let mut params = vec![
        ("q", q),
        ("count", max_results.clamp(1, MAX_COUNT).to_string()),
        ("text_decorations", "false".to_string()),
        ("result_filter", "web".to_string()),
    ];
    if let Some(days) = filters.recency_days {
        params.push(("freshness", brave_freshness(days, now_unix)));
    }
    params
}

/// `recency_days` → Brave `freshness`. Shortcut values only when the window
/// matches exactly (`pw` would widen a 3-day request to 7 days); any other
/// window becomes an explicit `YYYY-MM-DDtoYYYY-MM-DD` range ending today.
pub fn brave_freshness(days: u32, now_unix: u64) -> String {
    match days {
        0 | 1 => "pd".to_string(),
        7 => "pw".to_string(),
        31 => "pm".to_string(),
        365 => "py".to_string(),
        _ => format!("{}to{}", utc_date(now_unix, days), utc_date(now_unix, 0)),
    }
}

/// Normalize a Brave web search response into `Source`s labeled `brave`.
/// Items without an HTTP(S) URL or a title are dropped.
pub fn normalize_brave_results(raw: &Value) -> Vec<Source> {
    raw.get("web")
        .and_then(|web| web.get("results"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let url = item.get("url").and_then(Value::as_str)?.trim();
            if !is_http_url(url) {
                return None;
            }
            let title = item
                .get("title")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|title| !title.is_empty())?;
            let mut source = Source::new(url, "brave").with_title(title);
            if let Some(description) = item
                .get("description")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                source = source.with_description(description);
            }
            if let Some(date) = item
                .get("page_age")
                .or_else(|| item.get("age"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|text| !text.is_empty())
            {
                source = source.with_published_date(date);
            }
            Some(source)
        })
        .collect()
}

/// Drop results that violate the include/exclude domain lists. A host
/// matches a domain when it equals it or is a subdomain of it; a leading
/// `www.` is ignored on both sides.
pub fn enforce_domain_filters(sources: Vec<Source>, filters: &SearchFilters) -> Vec<Source> {
    let includes: Vec<String> = filters
        .include_domains
        .iter()
        .filter_map(|domain| normalize_domain(domain))
        .collect();
    let excludes: Vec<String> = filters
        .exclude_domains
        .iter()
        .filter_map(|domain| normalize_domain(domain))
        .collect();
    if includes.is_empty() && excludes.is_empty() {
        return sources;
    }
    sources
        .into_iter()
        .filter(|source| {
            let Some(host) = host_of(&source.url) else {
                return false;
            };
            let included = includes.is_empty() || includes.iter().any(|d| host_matches(&host, d));
            let excluded = excludes.iter().any(|d| host_matches(&host, d));
            included && !excluded
        })
        .collect()
}

fn is_http_url(url: &str) -> bool {
    url::Url::parse(url)
        .map(|parsed| matches!(parsed.scheme(), "http" | "https"))
        .unwrap_or(false)
}

fn host_of(url: &str) -> Option<String> {
    let parsed = url::Url::parse(url).ok()?;
    let host = parsed.host_str()?.to_ascii_lowercase();
    Some(host.strip_prefix("www.").unwrap_or(&host).to_string())
}

/// Accepts `example.com`, `www.example.com`, or a URL; returns the bare
/// lowercase host without `www.`.
fn normalize_domain(domain: &str) -> Option<String> {
    let trimmed = domain.trim().to_ascii_lowercase();
    if trimmed.is_empty() {
        return None;
    }
    let host = if trimmed.contains("://") {
        url::Url::parse(&trimmed).ok()?.host_str()?.to_string()
    } else {
        trimmed
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default()
            .to_string()
    };
    let host = host.strip_prefix("www.").unwrap_or(&host).to_string();
    (!host.is_empty()).then_some(host)
}

fn host_matches(host: &str, domain: &str) -> bool {
    host == domain || host.ends_with(&format!(".{domain}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const NOW: u64 = 1_793_678_706; // 2026-11-03T04:05:06Z

    fn param<'a>(params: &'a [(&'static str, String)], key: &str) -> Option<&'a str> {
        params
            .iter()
            .find(|(name, _)| *name == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn params_clamp_count_and_disable_decorations() {
        let params = brave_search_params("rust mcp", 50, &SearchFilters::default(), NOW);
        assert_eq!(param(&params, "q"), Some("rust mcp"));
        assert_eq!(param(&params, "count"), Some("20"));
        assert_eq!(param(&params, "text_decorations"), Some("false"));
        assert_eq!(param(&params, "freshness"), None);

        let params = brave_search_params("x", 0, &SearchFilters::default(), NOW);
        assert_eq!(param(&params, "count"), Some("1"));
    }

    #[test]
    fn domain_filters_become_site_operators() {
        let filters = SearchFilters {
            recency_days: None,
            include_domains: vec!["https://www.Docs.rs/path".to_string()],
            exclude_domains: vec!["example.com".to_string(), "spam.net".to_string()],
        };
        let params = brave_search_params("tokio", 5, &filters, NOW);
        assert_eq!(
            param(&params, "q"),
            Some("tokio site:docs.rs -site:example.com -site:spam.net")
        );

        let multi = SearchFilters {
            include_domains: vec!["a.com".to_string(), "b.com".to_string()],
            ..SearchFilters::default()
        };
        let params = brave_search_params("tokio", 5, &multi, NOW);
        assert_eq!(param(&params, "q"), Some("tokio"));
    }

    #[test]
    fn freshness_uses_shortcuts_only_for_exact_windows() {
        assert_eq!(brave_freshness(1, NOW), "pd");
        assert_eq!(brave_freshness(7, NOW), "pw");
        assert_eq!(brave_freshness(31, NOW), "pm");
        assert_eq!(brave_freshness(365, NOW), "py");
        assert_eq!(brave_freshness(3, NOW), "2026-10-31to2026-11-03");
    }

    #[test]
    fn normalize_keeps_titled_http_results_in_order() {
        let raw = json!({
            "web": { "results": [
                { "title": "Tokio", "url": "https://tokio.rs/", "description": "Async runtime", "page_age": "2026-10-01T00:00:00" },
                { "title": "", "url": "https://untitled.example/" },
                { "title": "FTP", "url": "ftp://files.example/" },
                { "title": "Docs", "url": "https://docs.rs/tokio", "age": "2 days ago" }
            ]}
        });
        let sources = normalize_brave_results(&raw);
        assert_eq!(sources.len(), 2);
        assert_eq!(sources[0].url, "https://tokio.rs/");
        assert_eq!(sources[0].provider, "brave");
        assert_eq!(sources[0].description.as_deref(), Some("Async runtime"));
        assert_eq!(
            sources[0].published_date.as_deref(),
            Some("2026-10-01T00:00:00")
        );
        assert_eq!(sources[1].published_date.as_deref(), Some("2 days ago"));
    }

    #[test]
    fn post_filter_enforces_include_and_exclude() {
        let sources = vec![
            Source::new("https://docs.rs/a", "brave"),
            Source::new("https://www.docs.rs/b", "brave"),
            Source::new("https://api.docs.rs/c", "brave"),
            Source::new("https://notdocs.rs/d", "brave"),
            Source::new("https://example.com/e", "brave"),
        ];
        let include = SearchFilters {
            include_domains: vec!["docs.rs".to_string()],
            ..SearchFilters::default()
        };
        let kept: Vec<String> = enforce_domain_filters(sources.clone(), &include)
            .into_iter()
            .map(|s| s.url)
            .collect();
        assert_eq!(
            kept,
            vec![
                "https://docs.rs/a",
                "https://www.docs.rs/b",
                "https://api.docs.rs/c"
            ]
        );

        let exclude = SearchFilters {
            exclude_domains: vec!["docs.rs".to_string()],
            ..SearchFilters::default()
        };
        let kept: Vec<String> = enforce_domain_filters(sources, &exclude)
            .into_iter()
            .map(|s| s.url)
            .collect();
        assert_eq!(kept, vec!["https://notdocs.rs/d", "https://example.com/e"]);
    }
}
