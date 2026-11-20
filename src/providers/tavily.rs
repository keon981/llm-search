use crate::clock::now_unix_seconds;
use crate::error::Result;
use crate::model::search::{
    DetailedSearch, FetchOptions, SearchDepth, SearchFilters, SearchOptions,
};
use crate::model::source::{
    truncate_with_marker, BatchFetch, CrawlResult, FetchedPage, MapResult, Source,
};
use crate::providers::country::tavily_country;
use crate::providers::http::{build_client, send_json};
use crate::providers::keyring::{is_key_scoped_status, KeyRing};
use reqwest::Client;
use serde_json::{json, Value};
use std::sync::Arc;
use std::time::Duration;

use crate::error::GrokSearchError;

#[derive(Clone)]
pub struct TavilyProvider {
    client: Client,
    api_url: String,
    keys: Arc<KeyRing>,
    /// Which key created each research task: Tavily scopes a task to the key
    /// that queued it, so polls must not rotate onto another key.
    research_keys: Arc<std::sync::Mutex<std::collections::HashMap<String, usize>>>,
}

impl TavilyProvider {
    pub fn new(api_url: impl Into<String>, api_key: impl Into<String>, timeout: Duration) -> Self {
        Self::with_client(build_client(timeout), api_url, api_key)
    }

    /// Construct with an externally provided `reqwest::Client`. Used by
    /// `SearchService::new` to share one tuned client across providers.
    ///
    /// `api_key` accepts a single key or a comma-separated list; multiple
    /// keys are used round-robin with automatic failover on key-scoped
    /// errors (401/403/429/432/433).
    pub fn with_client(
        client: Client,
        api_url: impl Into<String>,
        api_key: impl Into<String>,
    ) -> Self {
        Self {
            client,
            api_url: api_url.into().trim_end_matches('/').to_string(),
            keys: Arc::new(KeyRing::parse(&api_key.into())),
            research_keys: Arc::default(),
        }
    }

    pub async fn search(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<Source>> {
        let raw = self
            .post(
                "search",
                &tavily_search_request_body(query, max_results, filters, now_unix_seconds()),
            )
            .await?;
        Ok(normalize_tavily_results(&raw))
    }

    /// Router-mode search: honors depth, provider answer, and inline page
    /// text. `search` stays the basic, answer-less call the source chain uses.
    pub async fn search_with(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
        options: &SearchOptions,
    ) -> Result<DetailedSearch> {
        let raw = self
            .post(
                "search",
                &tavily_search_request_body_with(
                    query,
                    max_results,
                    filters,
                    options,
                    now_unix_seconds(),
                ),
            )
            .await?;
        Ok(parse_tavily_detailed(&raw, options.raw_content_max_chars))
    }

    pub async fn extract(&self, url: &str) -> Result<FetchedPage> {
        self.extract_with(url, &FetchOptions::default()).await
    }

    /// Extract with an explicit depth / format. `depth: None` omits
    /// `extract_depth`, leaving Tavily's default.
    ///
    /// Pages that build their content with JavaScript (e.g. 104.com.tw job
    /// pages) come back empty at the default depth but extract fine at
    /// `advanced`, so an empty result is retried once at `advanced` unless
    /// the caller already asked for it. The retry costs Tavily's advanced
    /// rate, and only runs when the first call returned nothing.
    pub async fn extract_with(&self, url: &str, options: &FetchOptions) -> Result<FetchedPage> {
        let timeout = extract_request_timeout(options);
        let raw = self
            .post_with_timeout(
                "extract",
                &tavily_extract_request_body(url, options),
                timeout,
            )
            .await?;
        match parse_tavily_extract(&raw) {
            Err(GrokSearchError::Provider(message)) if message == EMPTY_EXTRACT => {
                let Some(retry) = advanced_retry_options(options) else {
                    return Err(GrokSearchError::Provider(message));
                };
                let raw = self
                    .post_with_timeout(
                        "extract",
                        &tavily_extract_request_body(url, &retry),
                        timeout,
                    )
                    .await?;
                parse_tavily_extract(&raw)
            }
            other => other,
        }
    }

    /// Extract several URLs in one `/extract` call (Tavily accepts up to 20).
    /// Outcomes line up with `urls`. URLs that come back empty, in
    /// `failed_results`, or not at all are retried together once at
    /// `advanced` (same rule as [`extract_with`](Self::extract_with)), unless
    /// the first call already used `advanced`. A transport-level failure of
    /// the first call fails the whole batch; one of the retry keeps the
    /// first-call outcomes.
    pub async fn extract_batch(
        &self,
        urls: &[String],
        options: &FetchOptions,
        deadline: tokio::time::Instant,
    ) -> Result<BatchFetch> {
        let timeout = extract_request_timeout(options);
        let first = tokio::time::timeout_at(
            deadline,
            self.post_with_timeout(
                "extract",
                &tavily_extract_batch_body(urls, options),
                timeout,
            ),
        )
        .await
        .map_err(|_| {
            GrokSearchError::Timeout(
                "Tavily batch extract timed out (LLM_SEARCH_TIMEOUT_SECONDS)".to_string(),
            )
        })?;
        let raw = first?;
        let mut batch = parse_tavily_extract_batch(&raw, urls);
        let Some(retry) = advanced_retry_options(options) else {
            return Ok(batch);
        };
        let retry_idx: Vec<usize> = batch
            .outcomes
            .iter()
            .enumerate()
            .filter_map(|(idx, outcome)| outcome.is_err().then_some(idx))
            .collect();
        if retry_idx.is_empty() {
            return Ok(batch);
        }
        let retry_urls: Vec<String> = retry_idx.iter().map(|idx| urls[*idx].clone()).collect();
        // The retry gets whatever time is left; running out keeps the
        // first call's pages.
        let Ok(Ok(raw)) = tokio::time::timeout_at(
            deadline,
            self.post_with_timeout(
                "extract",
                &tavily_extract_batch_body(&retry_urls, &retry),
                timeout,
            ),
        )
        .await
        else {
            return Ok(batch);
        };
        let second = parse_tavily_extract_batch(&raw, &retry_urls);
        for (slot, outcome) in retry_idx.into_iter().zip(second.outcomes) {
            if outcome.is_ok() {
                batch.outcomes[slot] = outcome;
            }
        }
        batch.request_ids.extend(second.request_ids);
        batch.usage = add_usage(batch.usage, second.usage);
        Ok(batch)
    }

    pub async fn map(&self, url: &str, max_results: usize) -> Result<Vec<Source>> {
        let mut params = serde_json::Map::new();
        params.insert("limit".into(), json!(max_results));
        Ok(self.map_with(url, &params).await?.sources)
    }

    /// `POST /crawl` with the caller's native crawl fields. Like map, the
    /// call may run for Tavily's whole `timeout`.
    pub async fn crawl_with(
        &self,
        url: &str,
        params: &serde_json::Map<String, Value>,
    ) -> Result<CrawlResult> {
        let raw = self
            .post_with_timeout(
                "crawl",
                &tavily_map_body(url, params),
                Some(site_call_timeout(params)),
            )
            .await?;
        Ok(parse_tavily_crawl(&raw))
    }

    /// `POST /map` with the caller's native map fields (validated by the
    /// tool layer). The request may run for Tavily's whole `timeout`
    /// (default 150 s), so it gets its own timeout instead of the client's.
    pub async fn map_with(
        &self,
        url: &str,
        params: &serde_json::Map<String, Value>,
    ) -> Result<MapResult> {
        let raw = self
            .post_with_timeout(
                "map",
                &tavily_map_body(url, params),
                Some(site_call_timeout(params)),
            )
            .await?;
        let mut sources = normalize_tavily_results(&raw);
        if let Some(limit) = params.get("limit").and_then(Value::as_u64) {
            sources.truncate(limit as usize);
        }
        Ok(MapResult {
            sources,
            base_url: raw
                .get("base_url")
                .and_then(Value::as_str)
                .map(str::to_string),
            request_id: response_request_id(&raw),
            usage: raw.get("usage").cloned(),
        })
    }

    fn endpoint(&self, path: &str) -> String {
        format!("{}/{}", self.api_url, path.trim_start_matches('/'))
    }

    /// POST under the client's default timeout.
    async fn post(&self, path: &str, body: &Value) -> Result<Value> {
        self.post_with_timeout(path, body, None).await
    }

    /// POST with an optional per-request timeout that overrides the shared
    /// client's (Map / Crawl may legitimately run past the default).
    pub(crate) async fn post_with_timeout(
        &self,
        path: &str,
        body: &Value,
        timeout: Option<Duration>,
    ) -> Result<Value> {
        let endpoint = self.endpoint(path);
        self.send(|key| {
            let request = self.client.post(&endpoint).bearer_auth(key).json(body);
            match timeout {
                Some(limit) => request.timeout(limit),
                None => request,
            }
        })
        .await
    }

    /// `POST /research`: queue a research task; the response carries its
    /// `request_id` and `status`. The key that queued it is remembered so
    /// polls use the same key.
    pub async fn research_start(&self, body: &Value) -> Result<Value> {
        let endpoint = self.endpoint("research");
        let (raw, key_index) = self
            .send_indexed(|key| self.client.post(&endpoint).bearer_auth(key).json(body))
            .await
            .map_err(|err| research_error(err, None))?;
        if let Some(id) = response_request_id(&raw) {
            if let Ok(mut keys) = self.research_keys.lock() {
                keys.insert(id, key_index);
            }
        }
        Ok(raw)
    }

    /// `GET /research/{id}`: the task's status, and its report once
    /// completed. Polls with the key that queued the task; for a task this
    /// process did not start (resume after a restart), each key is tried
    /// until one knows the id.
    pub async fn research_get(&self, id: &str) -> Result<Value> {
        let endpoint = self.endpoint(&format!("research/{id}"));
        let known = self
            .research_keys
            .lock()
            .ok()
            .and_then(|keys| keys.get(id).copied());
        let candidates: Vec<usize> = match known {
            Some(index) => vec![index],
            None => (0..self.keys.len()).collect(),
        };
        let mut last = None;
        for index in candidates {
            let request = self.client.get(&endpoint).bearer_auth(self.keys.key(index));
            match send_json(request, "Tavily").await {
                Ok(raw) => {
                    if let Ok(mut keys) = self.research_keys.lock() {
                        keys.insert(id.to_string(), index);
                    }
                    return Ok(raw);
                }
                Err(failure) if failure.status == Some(404) => last = Some(failure.error),
                Err(failure) => return Err(research_error(failure.error, Some(id))),
            }
        }
        Err(research_error(
            last.unwrap_or_else(|| GrokSearchError::Provider("HTTP 404".to_string())),
            Some(id),
        ))
    }

    /// Send with round-robin key selection. On a key-scoped failure
    /// (401/403/429/432/433) the request is retried once per remaining key;
    /// any other failure — timeout, 5xx, parse — returns immediately.
    async fn send(&self, build: impl Fn(&str) -> reqwest::RequestBuilder) -> Result<Value> {
        self.send_indexed(build).await.map(|(value, _)| value)
    }

    /// [`send`](Self::send) that also reports which key index served.
    async fn send_indexed(
        &self,
        build: impl Fn(&str) -> reqwest::RequestBuilder,
    ) -> Result<(Value, usize)> {
        let attempts = self.keys.len();
        let start = self.keys.start();
        let mut last_error = None;
        for offset in 0..attempts {
            let index = (start + offset) % attempts;
            let key = self.keys.key(index);
            match send_json(build(key), "Tavily").await {
                Ok(value) => return Ok((value, index)),
                Err(failure) => {
                    let key_scoped = failure.status.is_some_and(is_key_scoped_status);
                    if key_scoped && offset + 1 < attempts {
                        eprintln!(
                            "llm-search: Tavily key {}/{} hit HTTP {}; rotating to next key",
                            (start + offset) % attempts + 1,
                            attempts,
                            failure.status.unwrap_or_default(),
                        );
                        last_error = Some(failure.error);
                        continue;
                    }
                    return Err(failure.error);
                }
            }
        }
        // Unreachable: the loop always returns on the final attempt. Kept as
        // a defensive fallback instead of unwrap/panic.
        Err(last_error.unwrap_or_else(|| {
            GrokSearchError::Provider("Tavily request failed with no attempts".to_string())
        }))
    }
}

/// Body for `POST /search`. The publish-date window maps onto Tavily's own
/// fields and never touches `topic`: `recency_days` becomes `start_date`
/// (N days ago), `time_range` / `start_date` / `end_date` pass through.
/// (llm-search used to send the legacy `days` plus `topic=news`, which turned
/// every date-filtered search into a news-only search.)
pub fn tavily_search_request_body(
    query: &str,
    max_results: usize,
    filters: &SearchFilters,
    now_unix: u64,
) -> Value {
    let mut body = json!({
        "query": query,
        "max_results": max_results,
        "include_answer": false,
    });
    let object = body.as_object_mut().expect("literal object");
    if let Some(days) = filters.recency_days {
        object.insert(
            "start_date".into(),
            json!(crate::clock::utc_date(now_unix, days)),
        );
    }
    if let Some(range) = filters.time_range {
        object.insert("time_range".into(), json!(range.as_str()));
    }
    if let Some(start) = &filters.start_date {
        object.insert("start_date".into(), json!(start));
    }
    if let Some(end) = &filters.end_date {
        object.insert("end_date".into(), json!(end));
    }
    if !filters.include_domains.is_empty() {
        object.insert("include_domains".into(), json!(filters.include_domains));
    }
    if !filters.exclude_domains.is_empty() {
        object.insert("exclude_domains".into(), json!(filters.exclude_domains));
    }
    body
}

/// [`tavily_search_request_body`] plus the router-mode options: `search_depth`,
/// `include_answer`, `include_raw_content` (as `"markdown"`), the shared hints,
/// and finally the caller's Tavily-only fields, which override mode defaults.
pub fn tavily_search_request_body_with(
    query: &str,
    max_results: usize,
    filters: &SearchFilters,
    options: &SearchOptions,
    now_unix: u64,
) -> Value {
    let mut body = tavily_search_request_body(query, max_results, filters, now_unix);
    let object = body.as_object_mut().expect("tavily body is an object");
    object.insert("search_depth".into(), json!(options.depth.as_str()));
    object.insert("include_answer".into(), json!(options.include_answer));
    if options.include_raw_content {
        object.insert("include_raw_content".into(), json!("markdown"));
    }
    let hints = &options.hints;
    if let Some(name) = hints.country.as_deref().and_then(tavily_country) {
        object.insert("country".into(), json!(name));
    }
    if let Some(language) = &hints.language {
        object.insert("language".into(), json!(language));
    }
    if let Some(safe) = hints.safe_search {
        object.insert("safe_search".into(), json!(safe));
    }
    if let Some(exact) = hints.exact_match {
        object.insert("exact_match".into(), json!(exact));
    }
    for (key, value) in &options.tavily {
        object.insert(key.clone(), value.clone());
    }
    // Tavily honors `country` only for the general topic.
    if object
        .get("topic")
        .and_then(Value::as_str)
        .is_some_and(|topic| topic != "general")
    {
        object.remove("country");
    }
    body
}

/// Hints Tavily received but cannot use: an unknown country, or a country
/// with a non-general topic. Reported as `warnings` when Tavily serves.
pub fn tavily_dropped_hints(options: &SearchOptions) -> Vec<String> {
    let Some(country) = options.hints.country.as_deref() else {
        return Vec::new();
    };
    if tavily_country(country).is_none() {
        return vec![format!(
            "country \"{country}\" has no Tavily equivalent and was not sent to Tavily"
        )];
    }
    match options.tavily.get("topic").and_then(Value::as_str) {
        Some(topic) if topic != "general" => vec![format!(
            "country is ignored by Tavily for topic \"{topic}\" (general only)"
        )],
        _ => Vec::new(),
    }
}

/// Name the research failures a caller can act on: Tavily asking for the
/// streaming API (not supported here), and an unknown task id on resume.
fn research_error(err: GrokSearchError, id: Option<&str>) -> GrokSearchError {
    let message = err.to_string();
    if message.contains("research_stream_required") {
        return GrokSearchError::Provider(
            "Tavily requires streaming for this research request (research_stream_required); llm-search does not support research streaming yet".to_string(),
        );
    }
    match id {
        Some(id) if message.contains("HTTP 404") => GrokSearchError::InvalidParams(format!(
            "unknown research_id \"{id}\" (Tavily returned 404)"
        )),
        _ => err,
    }
}

/// Body for `POST /extract` with one URL.
pub fn tavily_extract_request_body(url: &str, options: &FetchOptions) -> Value {
    tavily_extract_batch_body(&[url.to_string()], options)
}

/// Body for `POST /extract`: `urls`, `format`, `extract_depth` when set, and
/// the caller's further Tavily extract fields.
pub fn tavily_extract_batch_body(urls: &[String], options: &FetchOptions) -> Value {
    let mut body = json!({ "urls": urls, "format": options.format.as_str() });
    let object = body.as_object_mut().expect("literal object");
    if let Some(depth) = options.depth {
        object.insert("extract_depth".into(), json!(depth.as_str()));
    }
    for (key, value) in &options.tavily {
        object.insert(key.clone(), value.clone());
    }
    body
}

/// Per-request timeout for an extract call: Tavily's own `timeout` plus a
/// margin for the round trip, or the client default when unset.
fn extract_request_timeout(options: &FetchOptions) -> Option<Duration> {
    options
        .tavily_timeout()
        .map(|limit| limit + Duration::from_secs(10))
}

/// Sum two `usage` objects' `credits`; otherwise keep whichever is present.
pub fn add_usage(first: Option<Value>, second: Option<Value>) -> Option<Value> {
    let credits = |usage: &Option<Value>| usage.as_ref()?.get("credits")?.as_f64();
    match (credits(&first), credits(&second)) {
        (Some(a), Some(b)) => Some(json!({ "credits": a + b })),
        _ => second.or(first),
    }
}

/// URL identity for matching extract results to requested URLs: Tavily may
/// echo a URL with a trailing slash or a fragment removed.
fn url_match_key(url: &str) -> String {
    let trimmed = url.trim();
    match url::Url::parse(trimmed) {
        Ok(mut parsed) => {
            parsed.set_fragment(None);
            parsed.as_str().trim_end_matches('/').to_string()
        }
        Err(_) => trimmed.trim_end_matches('/').to_string(),
    }
}

/// One extracted page from a Tavily `results[]` item, or `None` when it
/// carries no text.
fn page_from_item(item: &Value) -> Option<FetchedPage> {
    let content = item
        .get("raw_content")
        .or_else(|| item.get("content"))
        .and_then(Value::as_str)
        .filter(|text| !text.trim().is_empty())?
        .to_string();
    Some(FetchedPage {
        content,
        title: item
            .get("title")
            .and_then(Value::as_str)
            .map(str::to_string)
            .filter(|text| !text.trim().is_empty()),
        images: item
            .get("images")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
        favicon: item
            .get("favicon")
            .and_then(Value::as_str)
            .map(str::to_string),
        ..FetchedPage::default()
    })
}

/// Parse a crawl response: pages in Tavily's order, plus empty pages and
/// `failed_results` as failures.
pub fn parse_tavily_crawl(raw: &Value) -> CrawlResult {
    let mut pages = Vec::new();
    let mut failed = Vec::new();
    for item in raw
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(url) = item.get("url").and_then(Value::as_str) else {
            continue;
        };
        match page_from_item(item) {
            Some(page) => pages.push((url.to_string(), page)),
            None => failed.push((url.to_string(), EMPTY_EXTRACT.to_string())),
        }
    }
    for item in raw
        .get("failed_results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        if let Some(url) = item.get("url").and_then(Value::as_str) {
            let reason = item
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("failed");
            failed.push((url.to_string(), format!("Tavily crawl failed: {reason}")));
        }
    }
    CrawlResult {
        base_url: raw
            .get("base_url")
            .and_then(Value::as_str)
            .map(str::to_string),
        pages,
        failed,
        request_id: response_request_id(raw),
        usage: raw.get("usage").cloned(),
    }
}

/// Parse a batch extract response into one outcome per requested URL, in
/// request order. Missing and empty pages become errors carrying Tavily's
/// `failed_results` reason when it gave one.
pub fn parse_tavily_extract_batch(raw: &Value, requested: &[String]) -> BatchFetch {
    let items = |key: &str| {
        raw.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut pages: std::collections::HashMap<String, Option<FetchedPage>> = Default::default();
    for item in items("results") {
        if let Some(url) = item.get("url").and_then(Value::as_str) {
            pages.insert(url_match_key(url), page_from_item(&item));
        }
    }
    let mut failures: std::collections::HashMap<String, String> = Default::default();
    for item in items("failed_results") {
        if let Some(url) = item.get("url").and_then(Value::as_str) {
            let reason = item
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("failed")
                .to_string();
            failures.insert(url_match_key(url), reason);
        }
    }
    let request_id = response_request_id(raw);
    let outcomes = requested
        .iter()
        .map(|url| {
            let key = url_match_key(url);
            match pages.get(&key) {
                Some(Some(page)) => Ok(FetchedPage {
                    request_id: request_id.clone(),
                    ..page.clone()
                }),
                Some(None) => Err(EMPTY_EXTRACT.to_string()),
                None => Err(match failures.get(&key) {
                    Some(reason) => format!("Tavily extract failed: {reason}"),
                    None => "Tavily extract returned no result for this URL".to_string(),
                }),
            }
        })
        .collect();
    BatchFetch {
        outcomes,
        request_ids: request_id.into_iter().collect(),
        usage: raw.get("usage").cloned(),
    }
}

/// Parse a router-mode search response: the provider answer (when present and
/// non-blank) plus normalized sources, each carrying its page text as inline
/// `content` when Tavily returned `raw_content`, capped at `raw_max_chars`.
pub fn parse_tavily_detailed(raw: &Value, raw_max_chars: usize) -> DetailedSearch {
    let answer = raw
        .get("answer")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .map(str::to_string);
    let raw_by_url: std::collections::HashMap<&str, &str> = raw
        .get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let url = item.get("url").and_then(Value::as_str)?;
            let text = item
                .get("raw_content")
                .and_then(Value::as_str)
                .filter(|text| !text.trim().is_empty())?;
            Some((url, text))
        })
        .collect();
    let sources = normalize_tavily_results(raw)
        .into_iter()
        .map(|mut source| {
            if let Some(text) = raw_by_url.get(source.url.as_str()) {
                source.content = Some(truncate_with_marker(text, raw_max_chars));
            }
            source
        })
        .collect();
    DetailedSearch {
        answer,
        sources,
        request_id: response_request_id(raw),
        usage: raw.get("usage").cloned(),
        images: raw
            .get("images")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    }
}

/// Tavily's per-request id, present on every endpoint's response.
pub fn response_request_id(raw: &Value) -> Option<String> {
    raw.get("request_id")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// Map body with only a result limit (the legacy `max_results` call).
pub fn tavily_map_request_body(url: &str, max_results: usize) -> Value {
    let mut params = serde_json::Map::new();
    params.insert("limit".into(), json!(max_results));
    tavily_map_body(url, &params)
}

/// Body for `POST /map` (and the shared part of `/crawl`): `url` plus the
/// caller's native fields. Unset fields are omitted so Tavily's defaults
/// apply (depth 1, breadth 20, limit 50).
pub fn tavily_map_body(url: &str, params: &serde_json::Map<String, Value>) -> Value {
    let mut body = json!({ "url": url });
    let object = body.as_object_mut().expect("literal object");
    for (key, value) in params {
        object.insert(key.clone(), value.clone());
    }
    body
}

/// Tavily's default map / crawl `timeout`, in seconds.
const SITE_CALL_DEFAULT_TIMEOUT_SECS: f64 = 150.0;

/// Request timeout for a map / crawl call: Tavily's `timeout` (or its
/// default) plus a round-trip margin.
pub fn site_call_timeout(params: &serde_json::Map<String, Value>) -> Duration {
    let seconds = params
        .get("timeout")
        .and_then(Value::as_f64)
        .unwrap_or(SITE_CALL_DEFAULT_TIMEOUT_SECS);
    Duration::from_secs_f64(seconds) + Duration::from_secs(10)
}

pub fn limit_tavily_results(mut sources: Vec<Source>, max_results: usize) -> Vec<Source> {
    sources.truncate(max_results);
    sources
}

/// Parse a Tavily extract response into content + metadata. The extract
/// endpoint returns `title` alongside `raw_content` (verified live; the docs'
/// sample response omits it) but no published-date field, so `published_date`
/// is always `None` here.
const EMPTY_EXTRACT: &str = "Tavily extract returned empty content";

/// Options for the one `advanced` retry after an empty extract, or `None`
/// when the first call already used `advanced`.
pub fn advanced_retry_options(options: &FetchOptions) -> Option<FetchOptions> {
    (options.depth != Some(SearchDepth::Advanced)).then_some(FetchOptions {
        depth: Some(SearchDepth::Advanced),
        ..options.clone()
    })
}

pub fn parse_tavily_extract(raw: &Value) -> Result<FetchedPage> {
    let page = raw
        .get("results")
        .and_then(Value::as_array)
        .and_then(|items| items.first())
        .and_then(page_from_item)
        .ok_or_else(|| GrokSearchError::Provider(EMPTY_EXTRACT.to_string()))?;
    Ok(FetchedPage {
        request_id: response_request_id(raw),
        usage: raw.get("usage").cloned(),
        ..page
    })
}

/// Search results whose Tavily relevance `score` falls below this are junk,
/// not evidence. Long natural-language queries can drift Tavily onto one
/// generic word — observed live with "latest rmcp Rust MCP SDK release
/// version and what changed": dictionary and news-portal pages for "latest"
/// all scored ≤ 0.04, while on-topic results for answerable queries score
/// ≥ 0.49. 0.1 clears the junk band ~2.5x with ~5x headroom below on-topic
/// results. Items with no score (map results, API drift) always pass.
const MIN_SEARCH_SCORE: f64 = 0.1;

/// Normalize a Tavily `search`/`map` response into `Source`s. Search items
/// scoring below [`MIN_SEARCH_SCORE`] are dropped so keyword-drift junk never
/// reaches the enrichment/fallback source lists; score-less items (the map
/// endpoint returns bare URL strings) are kept unconditionally.
pub fn normalize_tavily_results(raw: &Value) -> Vec<Source> {
    raw.get("results")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|item| {
            if let Some(url) = item.as_str() {
                return Some(Source::new(url, "tavily"));
            }
            let url = item.get("url").and_then(Value::as_str)?;
            if item
                .get("score")
                .and_then(Value::as_f64)
                .is_some_and(|score| score < MIN_SEARCH_SCORE)
            {
                return None;
            }
            let mut source = Source::new(url, "tavily");
            if let Some(title) = item.get("title").and_then(Value::as_str) {
                source = source.with_title(title);
            }
            if let Some(description) = item
                .get("content")
                .or_else(|| item.get("description"))
                .and_then(Value::as_str)
            {
                source = source.with_description(description);
            }
            if let Some(published_date) = item.get("published_date").and_then(Value::as_str) {
                source = source.with_published_date(published_date);
            }
            source.favicon = item
                .get("favicon")
                .and_then(Value::as_str)
                .map(str::to_string);
            Some(source)
        })
        .collect()
}

#[cfg(test)]
mod router_body_tests {
    use super::*;
    use crate::model::search::{ExtractFormat, SearchDepth};

    #[test]
    fn empty_extract_retries_once_at_advanced_keeping_format() {
        let default = FetchOptions::default();
        assert_eq!(
            advanced_retry_options(&default),
            Some(FetchOptions {
                depth: Some(SearchDepth::Advanced),
                format: default.format,
                ..FetchOptions::default()
            })
        );
        let basic_text = FetchOptions {
            depth: Some(SearchDepth::Basic),
            format: ExtractFormat::Text,
            ..FetchOptions::default()
        };
        assert_eq!(
            advanced_retry_options(&basic_text),
            Some(FetchOptions {
                depth: Some(SearchDepth::Advanced),
                format: ExtractFormat::Text,
                ..FetchOptions::default()
            })
        );
        let advanced = FetchOptions {
            depth: Some(SearchDepth::Advanced),
            ..FetchOptions::default()
        };
        assert_eq!(advanced_retry_options(&advanced), None);
    }

    #[test]
    fn empty_extract_error_message_is_stable() {
        let err = parse_tavily_extract(&serde_json::json!({ "results": [] })).unwrap_err();
        assert!(matches!(err, GrokSearchError::Provider(ref m) if m == EMPTY_EXTRACT));
    }

    #[test]
    fn deep_body_requests_answer_raw_content_and_advanced_depth() {
        let options = SearchOptions {
            depth: SearchDepth::Advanced,
            include_answer: true,
            include_raw_content: true,
            raw_content_max_chars: 6000,
            ..SearchOptions::default()
        };
        let body = tavily_search_request_body_with("q", 5, &SearchFilters::default(), &options, 0);
        assert_eq!(body["search_depth"], "advanced");
        assert_eq!(body["include_answer"], true);
        assert_eq!(body["include_raw_content"], "markdown");
        assert_eq!(body["max_results"], 5);
    }

    /// Requirement: a call with no new parameters sends exactly today's body.
    /// Pinned as whole-object equality so any added key fails here.
    #[test]
    fn default_bodies_are_pinned() {
        let filters = SearchFilters::default();
        assert_eq!(
            tavily_search_request_body("q", 5, &filters, 0),
            json!({ "query": "q", "max_results": 5, "include_answer": false })
        );
        assert_eq!(
            tavily_search_request_body_with("q", 5, &filters, &SearchOptions::default(), 0),
            json!({
                "query": "q", "max_results": 5, "include_answer": false,
                "search_depth": "basic"
            })
        );
        let deep = SearchOptions {
            depth: SearchDepth::Advanced,
            include_answer: true,
            include_raw_content: true,
            raw_content_max_chars: 6000,
            ..SearchOptions::default()
        };
        assert_eq!(
            tavily_search_request_body_with("q", 5, &filters, &deep, 0),
            json!({
                "query": "q", "max_results": 5, "include_answer": true,
                "search_depth": "advanced", "include_raw_content": "markdown"
            })
        );
        assert_eq!(
            tavily_extract_request_body("https://a.example/", &FetchOptions::default()),
            json!({ "urls": ["https://a.example/"], "format": "markdown" })
        );
        assert_eq!(
            tavily_map_request_body("https://a.example/", 10),
            json!({ "url": "https://a.example/", "limit": 10 })
        );
    }

    #[test]
    fn window_hints_and_tavily_fields_map_onto_the_body() {
        let filters = SearchFilters {
            time_range: Some(crate::model::search::TimeRange::Week),
            ..SearchFilters::default()
        };
        let mut tavily = serde_json::Map::new();
        tavily.insert("search_depth".into(), json!("fast"));
        tavily.insert("include_answer".into(), json!("advanced"));
        let options = SearchOptions {
            hints: crate::model::search::SearchHints {
                country: Some("TW".into()),
                language: Some("zh-tw".into()),
                exact_match: Some(true),
                safe_search: None,
            },
            tavily,
            ..SearchOptions::default()
        };
        let body = tavily_search_request_body_with("q", 5, &filters, &options, 0);
        assert_eq!(body["time_range"], "week");
        assert_eq!(body["country"], "taiwan");
        assert_eq!(body["language"], "zh-tw");
        assert_eq!(body["exact_match"], true);
        // Caller fields override the mode defaults.
        assert_eq!(body["search_depth"], "fast");
        assert_eq!(body["include_answer"], "advanced");
        assert!(body.get("topic").is_none());

        let dates = SearchFilters {
            start_date: Some("2026-01-01".into()),
            end_date: Some("2026-01-31".into()),
            ..SearchFilters::default()
        };
        let body = tavily_search_request_body("q", 5, &dates, 0);
        assert_eq!(body["start_date"], "2026-01-01");
        assert_eq!(body["end_date"], "2026-01-31");
    }

    #[test]
    fn country_is_dropped_for_non_general_topics_and_unknown_names() {
        let mut tavily = serde_json::Map::new();
        tavily.insert("topic".into(), json!("finance"));
        let options = SearchOptions {
            hints: crate::model::search::SearchHints {
                country: Some("US".into()),
                ..Default::default()
            },
            tavily,
            ..SearchOptions::default()
        };
        let body = tavily_search_request_body_with("q", 5, &SearchFilters::default(), &options, 0);
        assert!(body.get("country").is_none());
        assert_eq!(tavily_dropped_hints(&options).len(), 1);

        let unknown = SearchOptions {
            hints: crate::model::search::SearchHints {
                country: Some("XX".into()),
                ..Default::default()
            },
            ..SearchOptions::default()
        };
        let body = tavily_search_request_body_with("q", 5, &SearchFilters::default(), &unknown, 0);
        assert!(body.get("country").is_none());
        assert!(tavily_dropped_hints(&unknown)[0].contains("no Tavily equivalent"));
    }

    #[test]
    fn detailed_parse_keeps_request_id_usage_images_and_favicon() {
        let raw = json!({
            "request_id": "req-1",
            "usage": { "credits": 2 },
            "images": ["https://img.example/1.png"],
            "results": [{ "url": "https://a.example/", "title": "A", "score": 0.9, "favicon": "https://a.example/f.ico" }]
        });
        let parsed = parse_tavily_detailed(&raw, 0);
        assert_eq!(parsed.request_id.as_deref(), Some("req-1"));
        assert_eq!(parsed.usage, Some(json!({ "credits": 2 })));
        assert_eq!(parsed.images, vec![json!("https://img.example/1.png")]);
        assert_eq!(
            parsed.sources[0].favicon.as_deref(),
            Some("https://a.example/f.ico")
        );
    }

    #[test]
    fn batch_body_carries_urls_and_extract_fields() {
        let mut tavily = serde_json::Map::new();
        tavily.insert("query".into(), json!("pricing"));
        tavily.insert("include_images".into(), json!(true));
        let options = FetchOptions {
            depth: Some(SearchDepth::Advanced),
            tavily,
            ..FetchOptions::default()
        };
        let urls = vec![
            "https://a.example/".to_string(),
            "https://b.example/".to_string(),
        ];
        assert_eq!(
            tavily_extract_batch_body(&urls, &options),
            json!({
                "urls": ["https://a.example/", "https://b.example/"],
                "format": "markdown", "extract_depth": "advanced",
                "query": "pricing", "include_images": true
            })
        );
    }

    #[test]
    fn batch_parse_aligns_outcomes_with_requested_urls() {
        let raw = json!({
            "request_id": "req-9",
            "usage": { "credits": 1 },
            "results": [
                // Echoed without the trailing slash and with a fragment dropped.
                { "url": "https://a.example", "raw_content": "A text", "title": "A", "favicon": "https://a.example/f.ico" },
                { "url": "https://empty.example/", "raw_content": "  " }
            ],
            "failed_results": [ { "url": "https://blocked.example/", "error": "403 Forbidden" } ]
        });
        let requested = vec![
            "https://a.example/#top".to_string(),
            "https://empty.example/".to_string(),
            "https://blocked.example/".to_string(),
            "https://missing.example/".to_string(),
        ];
        let batch = parse_tavily_extract_batch(&raw, &requested);
        assert_eq!(batch.request_ids, vec!["req-9".to_string()]);
        assert_eq!(batch.usage, Some(json!({ "credits": 1 })));
        let first = batch.outcomes[0].as_ref().expect("a.example served");
        assert_eq!(first.content, "A text");
        assert_eq!(first.favicon.as_deref(), Some("https://a.example/f.ico"));
        assert_eq!(first.request_id.as_deref(), Some("req-9"));
        assert_eq!(batch.outcomes[1], Err(EMPTY_EXTRACT.to_string()));
        assert_eq!(
            batch.outcomes[2],
            Err("Tavily extract failed: 403 Forbidden".to_string())
        );
        assert!(batch.outcomes[3]
            .as_ref()
            .unwrap_err()
            .contains("no result"));
    }

    #[test]
    fn map_body_passes_native_fields_and_timeout_has_margin() {
        let mut params = serde_json::Map::new();
        params.insert("instructions".into(), json!("SDK pages"));
        params.insert("select_paths".into(), json!(["/sdk/.*"]));
        assert_eq!(
            tavily_map_body("docs.example.com", &params),
            json!({ "url": "docs.example.com", "instructions": "SDK pages", "select_paths": ["/sdk/.*"] })
        );
        assert_eq!(site_call_timeout(&params), Duration::from_secs(160));
        params.insert("timeout".into(), json!(30.0));
        assert_eq!(site_call_timeout(&params), Duration::from_secs(40));
    }

    #[test]
    fn crawl_parse_keeps_order_and_reports_empty_and_failed_pages() {
        let raw = json!({
            "base_url": "docs.example.com",
            "request_id": "req-c",
            "results": [
                { "url": "https://docs.example.com/a", "raw_content": "A" },
                { "url": "https://docs.example.com/empty", "raw_content": "" },
                { "url": "https://docs.example.com/b", "raw_content": "B", "favicon": "https://docs.example.com/f.ico" }
            ],
            "failed_results": [ { "url": "https://docs.example.com/c", "error": "timeout" } ]
        });
        let crawl = parse_tavily_crawl(&raw);
        assert_eq!(crawl.base_url.as_deref(), Some("docs.example.com"));
        assert_eq!(crawl.request_id.as_deref(), Some("req-c"));
        let urls: Vec<&str> = crawl.pages.iter().map(|(url, _)| url.as_str()).collect();
        assert_eq!(
            urls,
            vec!["https://docs.example.com/a", "https://docs.example.com/b"]
        );
        assert_eq!(
            crawl.pages[1].1.favicon.as_deref(),
            Some("https://docs.example.com/f.ico")
        );
        assert_eq!(crawl.failed.len(), 2);
        assert!(crawl.failed[1].1.contains("timeout"));
    }

    #[test]
    fn research_errors_are_named() {
        let stream = research_error(
            GrokSearchError::Provider(
                "Tavily returned HTTP 400 Bad Request: {\"detail\":{\"error_code\":\"research_stream_required\"}}"
                    .to_string(),
            ),
            None,
        );
        assert!(stream
            .to_string()
            .contains("does not support research streaming"));
        let missing = research_error(
            GrokSearchError::Provider("Tavily returned HTTP 404 Not Found: {}".to_string()),
            Some("abc"),
        );
        assert!(
            matches!(missing, GrokSearchError::InvalidParams(ref m) if m.contains("unknown research_id"))
        );
        let other = research_error(
            GrokSearchError::Provider("HTTP 500".to_string()),
            Some("abc"),
        );
        assert!(matches!(other, GrokSearchError::Provider(ref m) if m == "HTTP 500"));
    }

    #[test]
    fn usage_credits_add_up() {
        assert_eq!(
            add_usage(Some(json!({ "credits": 1 })), Some(json!({ "credits": 2 }))),
            Some(json!({ "credits": 3.0 }))
        );
        assert_eq!(
            add_usage(None, Some(json!({ "credits": 2 }))),
            Some(json!({ "credits": 2 }))
        );
        assert_eq!(add_usage(None, None), None);
    }

    #[test]
    fn quick_body_stays_basic_without_raw_content() {
        let body = tavily_search_request_body_with(
            "q",
            5,
            &SearchFilters::default(),
            &SearchOptions::default(),
            0,
        );
        assert_eq!(body["search_depth"], "basic");
        assert_eq!(body["include_answer"], false);
        assert!(body.get("include_raw_content").is_none());
    }

    #[test]
    fn extract_body_omits_depth_unless_requested() {
        let body = tavily_extract_request_body("https://a.example/", &FetchOptions::default());
        assert_eq!(body["format"], "markdown");
        assert!(body.get("extract_depth").is_none());

        let options = FetchOptions {
            depth: Some(SearchDepth::Advanced),
            format: ExtractFormat::Text,
            ..FetchOptions::default()
        };
        let body = tavily_extract_request_body("https://a.example/", &options);
        assert_eq!(body["format"], "text");
        assert_eq!(body["extract_depth"], "advanced");
    }

    #[test]
    fn detailed_parse_attaches_capped_raw_content_and_answer() {
        let raw = json!({
            "answer": "  Tokio is an async runtime.  ",
            "results": [
                { "url": "https://tokio.rs/", "title": "Tokio", "content": "snippet", "score": 0.9, "raw_content": "x".repeat(50) },
                { "url": "https://docs.rs/tokio", "title": "Docs", "content": "snippet", "score": 0.8 },
                { "url": "https://junk.example/", "title": "Junk", "score": 0.01, "raw_content": "junk" }
            ]
        });
        let parsed = parse_tavily_detailed(&raw, 30);
        assert_eq!(parsed.answer.as_deref(), Some("Tokio is an async runtime."));
        assert_eq!(parsed.sources.len(), 2);
        let first = parsed.sources[0].content.as_deref().unwrap();
        assert_eq!(first.chars().count(), 30);
        assert!(first.contains("truncated"));
        assert!(parsed.sources[1].content.is_none());
    }
}

#[cfg(test)]
mod key_ring_tests {
    use super::*;

    #[test]
    fn rotation_cursor_is_shared_across_provider_clones() {
        let provider =
            TavilyProvider::with_client(Client::new(), "https://api.tavily.com", "tvly-a,tvly-b");
        let clone = provider.clone();
        // The cursor is shared (Arc): the clone continues the sequence rather
        // than restarting, regardless of the randomized starting offset.
        let a = provider.keys.start();
        assert_eq!(clone.keys.start(), (a + 1) % 2);
        assert_eq!(provider.keys.start(), (a + 2) % 2);
    }
}
