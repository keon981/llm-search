use serde::{Deserialize, Serialize};

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
    /// Router mode only: results to return, clamped to 1..=10 (default 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_results: Option<usize>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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
}

/// One URL of a multi-URL `web_fetch` that produced no content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FetchFailure {
    pub url: String,
    pub error: String,
}

/// Multi-URL `web_fetch` (`urls` argument). `max_chars` is a budget shared by
/// all results, spent in input order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WebFetchBatchOutput {
    pub results: Vec<WebFetchOutput>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed: Vec<FetchFailure>,
    /// Provenance for every URL, successes and failures, in input order.
    pub source_log: Vec<SourceLogEntry>,
    /// True when the shared budget cut any result.
    pub truncated: bool,
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
        };
        let value = serde_json::to_value(&output).unwrap();
        assert_eq!(value["fallback_reason"], "github_issue empty render");
    }
}
