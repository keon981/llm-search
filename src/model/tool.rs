use serde::{Deserialize, Serialize};

use crate::model::search::SearchHints;
use crate::model::source::Source;
use crate::sources::SourceType;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WebSearchInput {
    pub query: String,
    pub platform: Option<String>,
    pub model: Option<String>,
    pub extra_sources: Option<usize>,
    pub recency_days: Option<u32>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include_domains: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude_domains: Vec<String>,
    pub include_content: Option<bool>,
    /// `"concise"` (answer + source metadata only) or `"detailed"` (inline
    /// content, subject to the response budget). When set, takes precedence
    /// over the legacy `include_content` flag.
    pub response_format: Option<String>,
    /// Router mode only (`LLM_SEARCH_PROVIDER=brave`): `"quick"` (default),
    /// `"deep"`, or `"verify"`. Ignored, with a warning, in Grok mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Router mode only: results to return, clamped to 1..=20 (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
    /// Relative publish window (`day` / `week` / `month` / `year`). Strict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_range: Option<String>,
    /// Absolute publish window bounds, `YYYY-MM-DD`. Strict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_date: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_date: Option<String>,
    /// Cross-provider ranking hints (country, language, safe_search, exact_match).
    #[serde(default, skip_serializing_if = "SearchHints::is_empty")]
    pub hints: SearchHints,
    /// Tavily-only search fields under their Tavily names, validated by the
    /// tool layer; applied only when Tavily serves.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub tavily: serde_json::Map<String, serde_json::Value>,
    /// `mode=research` fields under Tavily research names (`model`,
    /// `output_schema`, `citation_format`, `output_length`), validated by the
    /// tool layer.
    #[serde(default, skip_serializing_if = "serde_json::Map::is_empty")]
    pub research: serde_json::Map<String, serde_json::Value>,
    /// Resume waiting on an earlier research task instead of starting one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_id: Option<String>,
}

/// One provider that was tried and did not serve the result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchFallback {
    pub provider: String,
    pub error: String,
}

/// Provenance record for one returned source, cite-able by `source_id`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SourceLogEntry {
    /// `src_001`, `src_002`, … in returned order.
    pub source_id: String,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rank: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accessed_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extracted_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub content_hash: Option<String>,
    /// `"success"` or `"failed"`.
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WebSearchOutput {
    pub session_id: String,
    pub content: String,
    pub sources_count: usize,
    pub sources: Vec<Source>,
    pub search_provider: String,
    pub fallback_used: bool,
    pub fallback_reason: Option<String>,
    /// True when the response budget trimmed inline source content. The cache
    /// keeps the full text — recover via `get_sources` or `web_fetch`.
    pub truncated: bool,
    /// `"brave"` (router) or `"grok"` — the pipeline that served the call.
    pub engine: String,
    /// Router mode that ran (`quick` / `deep` / `verify`); absent in Grok mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<String>,
    /// Provider-generated answer (Tavily in deep / verify), when one exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer: Option<String>,
    /// Providers tried before the one that served the result, with why.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<SearchFallback>,
    /// Disclosure text whenever a fallback served the result.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// Provenance for every returned source, in returned order.
    pub source_log: Vec<SourceLogEntry>,
    /// Non-fatal problems with the request (an ignored parameter, …).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
    /// Provider request id (Tavily), when the serving provider returned one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    /// Provider credit usage (Tavily `include_usage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
    /// Query-level images (Tavily `include_images`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<serde_json::Value>,
    /// `mode=research`: the Tavily research task id (pass it back as
    /// `research_id` to keep waiting).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub research_id: Option<String>,
    /// `mode=research`: Tavily's task status (`completed`, or still running).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// `mode=research`: seconds this call waited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GetSourcesOutput {
    pub session_id: String,
    pub sources: Vec<Source>,
    /// Number of sources in THIS page (`sources.len()`), not the cache total.
    pub sources_count: usize,
    /// Total sources cached for the session, across all pages.
    pub total_sources: usize,
    pub offset: usize,
    /// Offset of the next page; absent when this page reaches the end.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_offset: Option<usize>,
    /// True when the response budget trimmed inline content within this page.
    pub truncated: bool,
}

/// Optional page metadata a provider returned (Tavily extract): flattened into
/// `WebFetchOutput`, every field absent from JSON unless set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct FetchExtras {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub favicon: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WebFetchOutput {
    pub url: String,
    pub content: String,
    pub original_length: usize,
    pub truncated: bool,
    /// Always present. `generic` on the fallback path (decision D-02);
    /// specialist values arrive in Phase 2.
    pub source_type: SourceType,
    /// Present only when a specialist was matched and then failed
    /// (decision D-01); omitted from JSON otherwise.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_reason: Option<String>,
    /// What produced the content: a source-chain provider (`tavily`, `exa`,
    /// …) or a specialist extractor (`github`, `stackexchange`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub domain: Option<String>,
    /// SHA-256 (hex) of the returned `content`.
    #[serde(default)]
    pub content_hash: String,
    /// UTC RFC 3339 time the content was extracted.
    #[serde(default)]
    pub extracted_at: String,
    /// One provenance record for this page.
    #[serde(default)]
    pub source_log: Vec<SourceLogEntry>,
    #[serde(flatten)]
    pub extras: FetchExtras,
}

/// One crawled page in the `web_crawl` index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CrawlPageIndex {
    pub url: String,
    /// Characters of extracted text.
    pub chars: usize,
    /// 1-based line where the page's section starts in `file_path`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
}

/// `web_crawl` result. On a local (stdio) server the pages are written to
/// `file_path` and only the index comes back; on the HTTP transport they come
/// back in `sources`, trimmed to the response budget, with the full set under
/// `session_id` for `get_sources`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WebCrawlOutput {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    pub pages_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_path: Option<String>,
    pub pages: Vec<CrawlPageIndex>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<Source>,
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<FetchFailure>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
}

/// One URL of a multi-URL `web_fetch` that produced no content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchFailure {
    pub url: String,
    pub error: String,
}

/// Multi-URL `web_fetch` (`urls` argument). `max_chars` is a budget shared by
/// all results, spent in input order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct WebFetchBatchOutput {
    pub results: Vec<WebFetchOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<FetchFailure>,
    /// Provenance for every URL, successes and failures, in input order.
    pub source_log: Vec<SourceLogEntry>,
    /// True when the shared budget cut any result.
    pub truncated: bool,
    /// Tavily batch extract request id(s), when the batch went to Tavily.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub request_ids: Vec<String>,
    /// Tavily credit usage of the batch call(s) (`include_usage`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sources::SourceType;

    #[test]
    fn web_fetch_output_includes_source_type_and_omits_none_fallback_reason() {
        let output = WebFetchOutput {
            url: "https://example.com".to_string(),
            content: "hi".to_string(),
            original_length: 2,
            truncated: false,
            source_type: SourceType::Generic,
            fallback_reason: None,
            provider: None,
            domain: None,
            content_hash: String::new(),
            extracted_at: String::new(),
            source_log: Vec::new(),
            ..WebFetchOutput::default()
        };
        let value = serde_json::to_value(&output).unwrap();
        assert_eq!(value["source_type"], "generic");
        assert!(value.get("fallback_reason").is_none());
    }

    #[test]
    fn web_fetch_output_serializes_fallback_reason_when_present() {
        let output = WebFetchOutput {
            url: "https://example.com".to_string(),
            content: "hi".to_string(),
            original_length: 2,
            truncated: false,
            source_type: SourceType::Generic,
            fallback_reason: Some("github_issue empty render".to_string()),
            provider: None,
            domain: None,
            content_hash: String::new(),
            extracted_at: String::new(),
            source_log: Vec::new(),
            ..WebFetchOutput::default()
        };
        let value = serde_json::to_value(&output).unwrap();
        assert_eq!(value["fallback_reason"], "github_issue empty render");
    }
}
