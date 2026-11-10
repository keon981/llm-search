use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchRequest {
    pub model: String,
    pub system: Option<String>,
    pub messages: Vec<SearchMessage>,
    pub tools: Vec<SearchTool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchMessage {
    pub role: String,
    pub content: Vec<ContentBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
}

impl ContentBlock {
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text { text: text.into() }
    }

    pub fn as_text(&self) -> &str {
        match self {
            Self::Text { text } => text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchTool {
    pub name: String,
    pub input_schema: Value,
}

impl SearchTool {
    pub fn web_search() -> Self {
        Self {
            name: "web_search".to_string(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchResponse {
    pub content: String,
    pub sources: Vec<crate::model::source::Source>,
}

/// Search depth for providers that offer more than one (Tavily).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SearchDepth {
    #[default]
    Basic,
    Advanced,
}

impl SearchDepth {
    pub fn as_str(&self) -> &'static str {
        match self {
            SearchDepth::Basic => "basic",
            SearchDepth::Advanced => "advanced",
        }
    }
}

/// Router-mode search options. Providers that cannot honor a field ignore it;
/// `SourceProvider::search_detailed`'s default does exactly that.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchOptions {
    pub depth: SearchDepth,
    /// Ask the provider for its own generated answer (Tavily `include_answer`).
    pub include_answer: bool,
    /// Ask for page text inline with each result (Tavily `include_raw_content`).
    pub include_raw_content: bool,
    /// Per-result cap on inline page text; 0 means no cap.
    pub raw_content_max_chars: usize,
}

/// Result of a router-mode search: sources plus an optional provider answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DetailedSearch {
    pub answer: Option<String>,
    pub sources: Vec<crate::model::source::Source>,
}

/// Output format for page extraction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ExtractFormat {
    #[default]
    Markdown,
    Text,
}

impl ExtractFormat {
    pub fn as_str(&self) -> &'static str {
        match self {
            ExtractFormat::Markdown => "markdown",
            ExtractFormat::Text => "text",
        }
    }
}

/// `web_fetch` options passed to the provider that extracts the page.
/// `depth: None` keeps the provider's default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FetchOptions {
    pub depth: Option<SearchDepth>,
    pub format: ExtractFormat,
}

/// Structured retrieval filters shared by source providers.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchFilters {
    pub recency_days: Option<u32>,
    pub include_domains: Vec<String>,
    pub exclude_domains: Vec<String>,
}

impl SearchFilters {
    pub fn is_empty(&self) -> bool {
        self.recency_days.is_none()
            && self.include_domains.is_empty()
            && self.exclude_domains.is_empty()
    }
}
