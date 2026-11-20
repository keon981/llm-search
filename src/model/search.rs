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
    /// Ranking hints shared across providers (Brave and Tavily map them).
    pub hints: SearchHints,
    /// Tavily-only search fields under their Tavily names, already validated.
    /// Merged last into the Tavily body, so they override the mode defaults.
    pub tavily: serde_json::Map<String, Value>,
}

/// Cross-provider ranking hints. Unlike [`SearchFilters`] these never make a
/// provider be skipped: one that cannot map a hint ignores it and the router
/// reports that in `warnings`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SearchHints {
    /// ISO 3166-1 alpha-2 code (upper case) or a full English country name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub country: Option<String>,
    /// Language code, forwarded as given (lower case).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub safe_search: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_match: Option<bool>,
}

impl SearchHints {
    pub fn is_empty(&self) -> bool {
        self.country.is_none()
            && self.language.is_none()
            && self.safe_search.is_none()
            && self.exact_match.is_none()
    }

    /// Names of the hints that are set, for "ignored by provider" warnings.
    pub fn names(&self) -> Vec<&'static str> {
        [
            ("country", self.country.is_some()),
            ("language", self.language.is_some()),
            ("safe_search", self.safe_search.is_some()),
            ("exact_match", self.exact_match.is_some()),
        ]
        .into_iter()
        .filter_map(|(name, set)| set.then_some(name))
        .collect()
    }
}

/// Result of a router-mode search: sources plus an optional provider answer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DetailedSearch {
    pub answer: Option<String>,
    pub sources: Vec<crate::model::source::Source>,
    /// Provider request id (Tavily `request_id`), for support tickets.
    pub request_id: Option<String>,
    /// Provider credit usage (Tavily `usage`, when `include_usage` was set).
    pub usage: Option<Value>,
    /// Query-level images (Tavily `images`, when `include_images` was set).
    pub images: Vec<Value>,
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
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FetchOptions {
    pub depth: Option<SearchDepth>,
    pub format: ExtractFormat,
    /// Further Tavily extract fields under their Tavily names (`query`,
    /// `chunks_per_source`, `include_images`, `include_favicon`, `timeout`,
    /// `include_usage`), validated by the tool layer. Other providers and the
    /// specialist extractors ignore them.
    pub tavily: serde_json::Map<String, Value>,
}

impl FetchOptions {
    /// Tavily's `timeout` (seconds) when the caller set one.
    pub fn tavily_timeout(&self) -> Option<std::time::Duration> {
        self.tavily
            .get("timeout")
            .and_then(Value::as_f64)
            .map(std::time::Duration::from_secs_f64)
    }
}

/// Relative publish-date window, as Tavily `time_range` / Brave `freshness`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TimeRange {
    Day,
    Week,
    Month,
    Year,
}

impl TimeRange {
    /// Accepts Tavily's spellings: `day` `week` `month` `year` or `d` `w` `m` `y`.
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "day" | "d" => Some(Self::Day),
            "week" | "w" => Some(Self::Week),
            "month" | "m" => Some(Self::Month),
            "year" | "y" => Some(Self::Year),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
            Self::Year => "year",
        }
    }

    /// Window length in days, matching Brave's `pd` / `pw` / `pm` / `py`.
    pub fn days(&self) -> u32 {
        match self {
            Self::Day => 1,
            Self::Week => 7,
            Self::Month => 31,
            Self::Year => 365,
        }
    }
}

/// Structured retrieval filters shared by source providers. Strict: a
/// provider that cannot honor a set field is skipped rather than ignoring it.
/// At most one of `recency_days`, `time_range`, `start_date`/`end_date` is set
/// (the tool layer rejects combinations).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SearchFilters {
    pub recency_days: Option<u32>,
    pub time_range: Option<TimeRange>,
    /// `YYYY-MM-DD`, validated by the tool layer.
    pub start_date: Option<String>,
    /// `YYYY-MM-DD`, validated by the tool layer.
    pub end_date: Option<String>,
    pub include_domains: Vec<String>,
    pub exclude_domains: Vec<String>,
}

impl SearchFilters {
    pub fn is_empty(&self) -> bool {
        self.recency_days.is_none()
            && self.time_range.is_none()
            && self.start_date.is_none()
            && self.end_date.is_none()
            && self.include_domains.is_empty()
            && self.exclude_domains.is_empty()
    }

    /// Earliest allowed publish day (days since the Unix epoch), from whichever
    /// window field is set.
    pub fn start_day(&self, now_unix: u64) -> Option<i64> {
        let today = (now_unix / 86_400) as i64;
        if let Some(days) = self.recency_days {
            return Some(today - i64::from(days));
        }
        if let Some(range) = self.time_range {
            return Some(today - i64::from(range.days()));
        }
        self.start_date
            .as_deref()
            .and_then(crate::clock::day_from_ymd)
    }

    /// Latest allowed publish day (days since the Unix epoch), if bounded.
    pub fn end_day(&self) -> Option<i64> {
        self.end_date
            .as_deref()
            .and_then(crate::clock::day_from_ymd)
    }
}
