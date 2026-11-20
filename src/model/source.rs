use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Source {
    pub url: String,
    pub provider: Cow<'static, str>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub published_date: Option<String>,
    /// Inline source content from the `resolve_content` pipeline.
    /// `None` → field absent from JSON (`include_content=false` path, backward-compat).
    /// `Some` → non-empty string: structured markdown or a deterministic failure note.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    /// Site icon URL (Tavily `include_favicon`); absent unless requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
}

impl Source {
    pub fn new(url: impl Into<String>, provider: impl Into<Cow<'static, str>>) -> Self {
        Self {
            url: url.into(),
            provider: provider.into(),
            title: None,
            description: None,
            published_date: None,
            content: None,
            favicon: None,
        }
    }

    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn with_description(mut self, description: impl Into<String>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn with_published_date(mut self, published_date: impl Into<String>) -> Self {
        self.published_date = Some(published_date.into());
        self
    }
}

/// A fetched page from a generic source provider: markdown content plus any
/// structured metadata the provider returned alongside it. Metadata fields are
/// best-effort — `None` whenever the provider response lacks them (Tavily
/// extract carries a title but no date; Firecrawl scrape carries both).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FetchedPage {
    pub content: String,
    pub title: Option<String>,
    pub published_date: Option<String>,
    /// Page images (Tavily `include_images`).
    pub images: Vec<serde_json::Value>,
    /// Site icon URL (Tavily `include_favicon`).
    pub favicon: Option<String>,
    /// Provider request id of the call that produced the page.
    pub request_id: Option<String>,
    /// Provider credit usage (Tavily `include_usage`).
    pub usage: Option<serde_json::Value>,
}

/// Result of a site crawl (`web_crawl`): extracted pages in crawl order,
/// pages that could not be read, and call metadata.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct CrawlResult {
    pub base_url: Option<String>,
    pub pages: Vec<(String, FetchedPage)>,
    pub failed: Vec<(String, String)>,
    pub request_id: Option<String>,
    pub usage: Option<serde_json::Value>,
}

/// Result of a site map (`web_map`): discovered URLs plus call metadata.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MapResult {
    pub sources: Vec<Source>,
    pub base_url: Option<String>,
    pub request_id: Option<String>,
    pub usage: Option<serde_json::Value>,
}

/// Result of a provider-side batch fetch: one outcome per requested URL, in
/// request order (`Err` carries the provider's reason), plus call metadata.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct BatchFetch {
    pub outcomes: Vec<std::result::Result<FetchedPage, String>>,
    pub request_ids: Vec<String>,
    pub usage: Option<serde_json::Value>,
}

impl FetchedPage {
    /// A page with content only — for providers/paths with no metadata.
    pub fn text(content: impl Into<String>) -> Self {
        Self {
            content: content.into(),
            ..Self::default()
        }
    }
}

/// Cap `text` at `max_chars` characters, ending with a marker that says how
/// much was cut. `max_chars == 0` means no cap. The marker counts toward the
/// cap, so the result never exceeds it.
pub fn truncate_with_marker(text: &str, max_chars: usize) -> String {
    let total = text.chars().count();
    if max_chars == 0 || total <= max_chars {
        return text.to_string();
    }
    let marker = format!("... [truncated {} chars]", total - max_chars);
    let marker_len = marker.chars().count();
    if marker_len >= max_chars {
        return text.chars().take(max_chars).collect();
    }
    let keep: String = text.chars().take(max_chars - marker_len).collect();
    format!("{keep}{marker}")
}

/// Upstream annotation payloads have been observed carrying the citation index
/// as the title (`"1"`, `"2"` — issue #21); single characters and purely
/// numeric strings are those artifacts, never real page titles.
pub fn is_junk_title(title: &str) -> bool {
    let trimmed = title.trim();
    trimmed.chars().count() <= 1 || trimmed.chars().all(|c| c.is_ascii_digit())
}

pub fn merge_sources(primary: Vec<Source>, secondary: Vec<Source>) -> Vec<Source> {
    let mut seen = HashSet::new();
    let mut merged = Vec::new();
    for source in primary.into_iter().chain(secondary) {
        if source.url.trim().is_empty() {
            continue;
        }
        if seen.insert(source.url.clone()) {
            merged.push(source);
        }
    }
    merged
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_with_marker_respects_the_cap() {
        assert_eq!(truncate_with_marker("short", 100), "short");
        assert_eq!(truncate_with_marker("anything", 0), "anything");
        let long = "x".repeat(200);
        let cut = truncate_with_marker(&long, 100);
        assert_eq!(cut.chars().count(), 100);
        assert!(cut.ends_with("... [truncated 100 chars]"));
    }

    #[test]
    fn source_content_none_omitted_from_json() {
        let source = Source::new("https://example.com", "tavily");
        let value = serde_json::to_value(&source).unwrap();
        // D-05: None must produce NO key, not "content": null.
        assert!(value.get("content").is_none());
    }

    #[test]
    fn source_content_some_appears_in_json() {
        let mut source = Source::new("https://example.com", "tavily");
        source.content = Some("hello".to_string());
        let value = serde_json::to_value(&source).unwrap();
        assert_eq!(value["content"], "hello");
        assert!(!value["content"].as_str().unwrap().is_empty());
    }
}
