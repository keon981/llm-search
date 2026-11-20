use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::cache::SourceCache;
use crate::config::{AuthMode, Config, SearchEngine};
use crate::credentials::{OAuthCredential, StaticApiKeyCredential};
use crate::error::{GrokSearchError, Result};
use crate::model::search::{
    ContentBlock, DetailedSearch, FetchOptions, SearchFilters, SearchMessage, SearchOptions,
    SearchRequest, SearchResponse, SearchTool,
};
use crate::model::source::{is_junk_title, merge_sources, FetchedPage, Source};
use crate::model::tool::{
    FetchFailure, GetSourcesOutput, SearchFallback, SourceLogEntry, WebFetchBatchOutput,
    WebFetchOutput, WebSearchInput, WebSearchOutput,
};
use crate::providers::brave::BraveProvider;
use crate::providers::exa::ExaProvider;
use crate::providers::firecrawl::FirecrawlProvider;
use crate::providers::grok::GrokResponsesProvider;
use crate::providers::tavily::TavilyProvider;
use crate::providers::tinyfish::TinyfishProvider;

#[async_trait]
pub trait AiProvider: Send + Sync {
    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse>;
}

#[async_trait]
pub trait SourceProvider: Send + Sync {
    async fn search_sources(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<Source>>;
    async fn fetch(&self, url: &str) -> Result<FetchedPage>;
    async fn map(&self, url: &str, max_results: usize) -> Result<Vec<Source>>;

    /// Router-mode search. Providers with no depth / answer / inline-text
    /// options (all but Tavily) serve a plain search and no answer.
    async fn search_detailed(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
        _options: &SearchOptions,
    ) -> Result<DetailedSearch> {
        Ok(DetailedSearch {
            sources: self.search_sources(query, max_results, filters).await?,
            ..DetailedSearch::default()
        })
    }

    /// `web_fetch` with extraction options. Providers with no depth / format
    /// options (all but Tavily) ignore them.
    async fn fetch_with(&self, url: &str, _options: &FetchOptions) -> Result<FetchedPage> {
        self.fetch(url).await
    }

    /// Site map with native options. Providers without them map with the
    /// `limit` alone (default 50, Tavily's default).
    async fn map_with(
        &self,
        url: &str,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::model::source::MapResult> {
        let limit = params
            .get("limit")
            .and_then(serde_json::Value::as_u64)
            .unwrap_or(50) as usize;
        Ok(crate::model::source::MapResult {
            sources: self.map(url, limit).await?,
            ..Default::default()
        })
    }

    /// Queue a research task (Tavily `POST /research`). `None` means the
    /// provider has no research endpoint.
    async fn research_start(&self, _body: &serde_json::Value) -> Option<Result<serde_json::Value>> {
        None
    }

    /// Poll a research task (Tavily `GET /research/{id}`).
    async fn research_get(&self, _id: &str) -> Option<Result<serde_json::Value>> {
        None
    }

    /// Site crawl with native options. `None` means the provider has no
    /// crawl endpoint.
    async fn crawl(
        &self,
        _url: &str,
        _params: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<crate::model::source::CrawlResult>> {
        None
    }

    /// Fetch several URLs in one provider call. `None` means the provider
    /// has no batch endpoint and the caller fetches URL by URL instead.
    /// The call must finish by `deadline`; a provider that retries keeps
    /// what its first call read when the retry runs out of time.
    async fn fetch_batch(
        &self,
        _urls: &[String],
        _options: &FetchOptions,
        _deadline: tokio::time::Instant,
    ) -> Option<Result<crate::model::source::BatchFetch>> {
        None
    }
}

#[async_trait]
impl AiProvider for GrokResponsesProvider {
    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        GrokResponsesProvider::search(self, request).await
    }
}

#[async_trait]
impl AiProvider for crate::providers::openai_compatible::OpenAICompatProvider {
    async fn search(&self, request: &SearchRequest) -> Result<SearchResponse> {
        crate::providers::openai_compatible::OpenAICompatProvider::search(self, request).await
    }
}

#[async_trait]
impl SourceProvider for TavilyProvider {
    async fn search_sources(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<Source>> {
        self.search(query, max_results, filters).await
    }

    async fn fetch(&self, url: &str) -> Result<FetchedPage> {
        self.extract(url).await
    }

    async fn map(&self, url: &str, max_results: usize) -> Result<Vec<Source>> {
        self.map(url, max_results).await
    }

    async fn search_detailed(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
        options: &SearchOptions,
    ) -> Result<DetailedSearch> {
        self.search_with(query, max_results, filters, options).await
    }

    async fn fetch_with(&self, url: &str, options: &FetchOptions) -> Result<FetchedPage> {
        self.extract_with(url, options).await
    }

    async fn fetch_batch(
        &self,
        urls: &[String],
        options: &FetchOptions,
        deadline: tokio::time::Instant,
    ) -> Option<Result<crate::model::source::BatchFetch>> {
        Some(self.extract_batch(urls, options, deadline).await)
    }

    async fn map_with(
        &self,
        url: &str,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::model::source::MapResult> {
        TavilyProvider::map_with(self, url, params).await
    }

    async fn crawl(
        &self,
        url: &str,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Option<Result<crate::model::source::CrawlResult>> {
        Some(self.crawl_with(url, params).await)
    }

    async fn research_start(&self, body: &serde_json::Value) -> Option<Result<serde_json::Value>> {
        Some(TavilyProvider::research_start(self, body).await)
    }

    async fn research_get(&self, id: &str) -> Option<Result<serde_json::Value>> {
        Some(TavilyProvider::research_get(self, id).await)
    }
}

#[async_trait]
impl SourceProvider for BraveProvider {
    async fn search_sources(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<Source>> {
        self.search(query, max_results, filters).await
    }

    async fn search_detailed(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
        options: &SearchOptions,
    ) -> Result<DetailedSearch> {
        Ok(DetailedSearch {
            sources: self
                .search_with_hints(query, max_results, filters, &options.hints)
                .await?,
            ..DetailedSearch::default()
        })
    }

    async fn fetch(&self, _url: &str) -> Result<FetchedPage> {
        // Never routed here (BRAVE_SPEC.supports_fetch is false); kept as an
        // explicit error rather than a silent empty page.
        Err(GrokSearchError::Provider(
            "Brave Search has no page-fetch endpoint".to_string(),
        ))
    }

    async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
        Err(GrokSearchError::Provider(
            "Brave Search has no site-map endpoint".to_string(),
        ))
    }
}

#[async_trait]
impl SourceProvider for FirecrawlProvider {
    async fn search_sources(
        &self,
        query: &str,
        max_results: usize,
        _filters: &SearchFilters,
    ) -> Result<Vec<Source>> {
        // Firecrawl search has no structured recency/domain filter; ignore filters.
        FirecrawlProvider::search(self, query, max_results).await
    }

    async fn fetch(&self, url: &str) -> Result<FetchedPage> {
        FirecrawlProvider::scrape(self, url).await
    }

    async fn map(&self, url: &str, max_results: usize) -> Result<Vec<Source>> {
        FirecrawlProvider::search(self, url, max_results).await
    }
}

#[async_trait]
impl SourceProvider for TinyfishProvider {
    async fn search_sources(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<Source>> {
        self.search(query, max_results, filters).await
    }

    async fn fetch(&self, url: &str) -> Result<FetchedPage> {
        TinyfishProvider::fetch(self, url).await
    }

    async fn map(&self, url: &str, max_results: usize) -> Result<Vec<Source>> {
        // No native site-map endpoint; a search on the URL is the closest
        // discovery primitive (mirrors the Firecrawl impl above).
        self.search(url, max_results, &SearchFilters::default())
            .await
    }
}

#[async_trait]
impl SourceProvider for ExaProvider {
    async fn search_sources(
        &self,
        query: &str,
        max_results: usize,
        filters: &SearchFilters,
    ) -> Result<Vec<Source>> {
        self.search(query, max_results, filters).await
    }

    async fn fetch(&self, url: &str) -> Result<FetchedPage> {
        ExaProvider::fetch(self, url).await
    }

    async fn map(&self, url: &str, max_results: usize) -> Result<Vec<Source>> {
        // No native site-map endpoint; a search on the URL is the closest
        // discovery primitive (mirrors the Firecrawl impl above).
        self.search(url, max_results, &SearchFilters::default())
            .await
    }
}

/// Static description of one known source provider: how it is configured, how
/// its absence is reported, and which request shapes it can honor. The
/// service's source chain (`SourceSlot` list) is built from these at
/// construction time.
pub(crate) struct ProviderSpec {
    pub(crate) name: &'static str,
    display: &'static str,
    enable_var: &'static str,
    key_var: &'static str,
    header: &'static str,
    /// Whether the provider honors `SearchFilters` (recency + domain
    /// include/exclude). Providers that cannot are *skipped* for filtered
    /// requests instead of silently violating the filter contract.
    supports_filters: bool,
    /// Whether the provider can honor an upper publish-date bound
    /// (`end_date`). TinyFish only knows "the last N minutes".
    supports_end_date: bool,
    /// Whether the provider maps the shared ranking hints (country, language,
    /// safe_search, exact_match). Others ignore them with a warning.
    honors_hints: bool,
    /// Whether the provider exposes a real site-map endpoint (`web_map`).
    supports_map: bool,
    /// Whether the provider can fetch a page (`web_fetch`, inline
    /// enrichment). Search-only providers (Brave) are skipped there.
    supports_fetch: bool,
    /// Core providers appear in zero-source diagnostics even when
    /// unconfigured; optional ones are mentioned only when the operator names
    /// them in `LLM_SEARCH_SOURCE_PROVIDERS`. Keeps "I never set up Exa"
    /// from reading as a broken credential in every failure message.
    core: bool,
}

/// The strict filters a `web_search` input carries. `time_range` was
/// validated by the tool layer, so an unparsable value cannot reach here.
fn search_filters(input: &WebSearchInput) -> SearchFilters {
    SearchFilters {
        recency_days: input.recency_days,
        time_range: input
            .time_range
            .as_deref()
            .and_then(crate::model::search::TimeRange::parse),
        start_date: input.start_date.clone(),
        end_date: input.end_date.clone(),
        include_domains: input.include_domains.clone(),
        exclude_domains: input.exclude_domains.clone(),
    }
}

/// Whether `spec` can serve a request carrying `filters` without violating
/// them. Filtered requests are filter-capable-providers-or-nothing.
fn filters_honored(spec: &ProviderSpec, filters: &SearchFilters) -> bool {
    filters.is_empty()
        || (spec.supports_filters && (filters.end_date.is_none() || spec.supports_end_date))
}

const TAVILY_SPEC: ProviderSpec = ProviderSpec {
    name: "tavily",
    display: "Tavily",
    enable_var: "TAVILY_ENABLED",
    key_var: "TAVILY_API_KEY",
    header: "x-tavily-api-key",
    supports_filters: true,
    supports_end_date: true,
    honors_hints: true,
    supports_map: true,
    supports_fetch: true,
    core: true,
};

const EXA_SPEC: ProviderSpec = ProviderSpec {
    name: "exa",
    display: "Exa",
    enable_var: "EXA_ENABLED",
    key_var: "EXA_API_KEY",
    header: "x-exa-api-key",
    supports_filters: true,
    supports_end_date: true,
    honors_hints: false,
    supports_map: false,
    supports_fetch: true,
    core: false,
};

const TINYFISH_SPEC: ProviderSpec = ProviderSpec {
    name: "tinyfish",
    display: "TinyFish",
    enable_var: "TINYFISH_ENABLED",
    key_var: "TINYFISH_API_KEY",
    header: "x-tinyfish-api-key",
    supports_filters: true,
    supports_end_date: false,
    honors_hints: false,
    supports_map: false,
    supports_fetch: true,
    core: false,
};

const FIRECRAWL_SPEC: ProviderSpec = ProviderSpec {
    name: "firecrawl",
    display: "Firecrawl",
    enable_var: "FIRECRAWL_ENABLED",
    key_var: "FIRECRAWL_API_KEY",
    header: "x-firecrawl-api-key",
    supports_filters: false,
    supports_end_date: false,
    honors_hints: false,
    supports_map: false,
    supports_fetch: true,
    core: true,
};

/// Brave Search: search only (no fetch, no map). Domain/recency filters are
/// honored via `freshness`, `site:` operators, and a host post-filter.
/// Not `core`: an unconfigured Brave is not a chain gap in Grok mode, and the
/// router reports it on its own terms.
const BRAVE_SPEC: ProviderSpec = ProviderSpec {
    name: "brave",
    display: "Brave",
    enable_var: "BRAVE_ENABLED",
    key_var: "BRAVE_API_KEY",
    header: "x-brave-api-key",
    supports_filters: true,
    supports_end_date: true,
    honors_hints: true,
    supports_map: false,
    supports_fetch: false,
    core: false,
};

/// Canonical chain order. Tavily's RAG-tuned results keep the primary slot;
/// Exa (semantic search, native filter support) outranks the keyword engines
/// among the newcomers; TinyFish is the free keyword/fetch tier; Firecrawl
/// keeps its historical last-resort slot among the fetch-capable providers
/// because its search cannot honor filters. Brave sits last so Grok mode's
/// enrichment/fallback behaves exactly as before unless every other provider
/// came up empty; router mode (`LLM_SEARCH_PROVIDER=brave`) sets its own
/// order. `LLM_SEARCH_SOURCE_PROVIDERS` overrides this order entirely.
const CANONICAL_SOURCE_ORDER: [&ProviderSpec; 5] = [
    &TAVILY_SPEC,
    &EXA_SPEC,
    &TINYFISH_SPEC,
    &FIRECRAWL_SPEC,
    &BRAVE_SPEC,
];

/// One instantiated provider in the source chain.
#[derive(Clone)]
struct SourceEntry {
    spec: &'static ProviderSpec,
    provider: Arc<dyn SourceProvider>,
}

/// One position in the configured source chain: an instantiated provider, or
/// a known-but-absent provider remembered so zero-source failures can name
/// what was missing and how to supply it (`enabled` decides whether the note
/// reads as "switched off" or "no key").
enum SourceSlot {
    Active(SourceEntry),
    Missing {
        spec: &'static ProviderSpec,
        enabled: bool,
    },
}

fn instantiate_source(
    spec: &'static ProviderSpec,
    config: &Config,
    http: &reqwest::Client,
) -> Option<Arc<dyn SourceProvider>> {
    match spec.name {
        "tavily" => config
            .tavily_enabled
            .then(|| config.tavily_api_key.clone())
            .flatten()
            .map(|key| {
                Arc::new(TavilyProvider::with_client(
                    http.clone(),
                    config.tavily_api_url.clone(),
                    key,
                )) as Arc<dyn SourceProvider>
            }),
        "exa" => config
            .exa_enabled
            .then(|| config.exa_api_key.clone())
            .flatten()
            .map(|key| {
                Arc::new(ExaProvider::with_client(
                    http.clone(),
                    config.exa_api_url.clone(),
                    key,
                )) as Arc<dyn SourceProvider>
            }),
        "tinyfish" => config
            .tinyfish_enabled
            .then(|| config.tinyfish_api_key.clone())
            .flatten()
            .map(|key| {
                Arc::new(TinyfishProvider::with_client(
                    http.clone(),
                    config.tinyfish_search_api_url.clone(),
                    config.tinyfish_fetch_api_url.clone(),
                    key,
                )) as Arc<dyn SourceProvider>
            }),
        "firecrawl" => config
            .firecrawl_enabled
            .then(|| config.firecrawl_api_key.clone())
            .flatten()
            .map(|key| {
                Arc::new(FirecrawlProvider::with_client(
                    http.clone(),
                    config.firecrawl_api_url.clone(),
                    key,
                )) as Arc<dyn SourceProvider>
            }),
        "brave" => config
            .brave_enabled
            .then(|| config.brave_api_key.clone())
            .flatten()
            .map(|key| {
                Arc::new(BraveProvider::with_client(
                    http.clone(),
                    config.brave_api_url.clone(),
                    key,
                )) as Arc<dyn SourceProvider>
            }),
        _ => None,
    }
}

fn provider_enabled(spec: &ProviderSpec, config: &Config) -> bool {
    match spec.name {
        "tavily" => config.tavily_enabled,
        "exa" => config.exa_enabled,
        "tinyfish" => config.tinyfish_enabled,
        "firecrawl" => config.firecrawl_enabled,
        "brave" => config.brave_enabled,
        _ => false,
    }
}

/// Whether `spec`'s key is configured, independent of its enable flag.
fn provider_key_set(spec: &ProviderSpec, config: &Config) -> bool {
    match spec.name {
        "tavily" => config.tavily_api_key.is_some(),
        "exa" => config.exa_api_key.is_some(),
        "tinyfish" => config.tinyfish_api_key.is_some(),
        "firecrawl" => config.firecrawl_api_key.is_some(),
        "brave" => config.brave_api_key.is_some(),
        _ => false,
    }
}

/// Resolve the effective source chain. Default: the canonical order over
/// whatever is configured, with core providers (Tavily/Firecrawl) holding
/// `Missing` slots when absent — preserving their long-standing place in
/// zero-source diagnostics. With an explicit `LLM_SEARCH_SOURCE_PROVIDERS`
/// list, exactly the named providers are slotted, and every named-but-absent
/// one gets a `Missing` slot: the operator asked for it, so its absence is
/// worth reporting.
/// Check every name in `LLM_SEARCH_SOURCE_PROVIDERS` against the known
/// provider registry. Exposed separately from [`build_source_slots`] so the
/// HTTP transport can fail at startup (before binding the listener) instead
/// of failing every request later — the operator's chain is operator-fixed
/// config, never a per-request header, so a startup check covers it fully.
pub fn validate_source_providers(config: &Config) -> Result<()> {
    let lists = [
        ("LLM_SEARCH_SOURCE_PROVIDERS", &config.source_providers),
        ("LLM_SEARCH_QUICK_ORDER", &config.quick_order),
        ("LLM_SEARCH_DEEP_ORDER", &config.deep_order),
    ];
    for (var, names) in lists {
        for name in names {
            if !CANONICAL_SOURCE_ORDER.iter().any(|spec| spec.name == name) {
                return Err(GrokSearchError::InvalidParams(format!(
                    "unknown source provider \"{name}\" in {var} (valid: tavily, exa, tinyfish, firecrawl, brave)"
                )));
            }
        }
    }
    Ok(())
}

/// Provider order for `web_search` `mode=quick` / `mode=deep` over the active
/// providers. Default: quick tries Brave then Tavily, deep Tavily then Brave,
/// then the rest of the chain in chain order. `LLM_SEARCH_QUICK_ORDER` /
/// `LLM_SEARCH_DEEP_ORDER` move the providers they name to the front in that
/// order; unlisted providers keep their default order behind them, and listed
/// providers that are not configured are skipped.
fn router_mode_order(
    active: &[&'static str],
    mode: RouterMode,
    config: &Config,
) -> Vec<&'static str> {
    let (head, custom) = match mode {
        RouterMode::Deep => (["tavily", "brave"], &config.deep_order),
        RouterMode::Quick | RouterMode::Verify => (["brave", "tavily"], &config.quick_order),
    };
    let default: Vec<&'static str> = head
        .into_iter()
        .filter(|name| active.contains(name))
        .chain(active.iter().copied().filter(|name| !head.contains(name)))
        .collect();
    let listed: Vec<&'static str> = custom
        .iter()
        .filter_map(|name| default.iter().copied().find(|known| known == name))
        .collect();
    let unlisted: Vec<&'static str> = default
        .into_iter()
        .filter(|name| !listed.contains(name))
        .collect();
    listed.into_iter().chain(unlisted).collect()
}

fn build_source_slots(config: &Config, http: &reqwest::Client) -> Result<Vec<SourceSlot>> {
    validate_source_providers(config)?;
    let explicit = !config.source_providers.is_empty();
    let specs: Vec<&'static ProviderSpec> = if explicit {
        config
            .source_providers
            .iter()
            .filter_map(|name| {
                CANONICAL_SOURCE_ORDER
                    .iter()
                    .copied()
                    .find(|spec| spec.name == name)
            })
            .collect()
    } else {
        CANONICAL_SOURCE_ORDER.to_vec()
    };

    let mut slots = Vec::new();
    for spec in specs {
        match instantiate_source(spec, config, http) {
            Some(provider) => slots.push(SourceSlot::Active(SourceEntry { spec, provider })),
            None if spec.core || explicit => slots.push(SourceSlot::Missing {
                spec,
                enabled: provider_enabled(spec, config),
            }),
            None => {}
        }
    }
    Ok(slots)
}

#[derive(Clone)]
pub struct SearchService {
    config: Config,
    /// The Grok / OpenAI-compatible answer provider. Required in Grok mode;
    /// optional in router mode, where it is never called (`None` when its
    /// config is absent or invalid — `ai_unavailable` says why).
    ai: Option<Arc<dyn AiProvider>>,
    ai_unavailable: Option<String>,
    /// Model name written into every `SearchRequest` produced by the service.
    /// Resolved once from `config` at construction so each transport gets the
    /// `grok_model` for either protocol. Per-call overrides via
    /// `WebSearchInput.model` still win.
    default_model: String,
    /// Ordered source-provider chain (first usable answer wins), with
    /// `Missing` placeholders so diagnostics can name absent providers.
    /// Behind `Arc` so `SearchService: Clone` stays cheap.
    source_slots: Arc<Vec<SourceSlot>>,
    cache: Arc<Mutex<SourceCache>>,
    /// Shared reqwest client for the sources pipeline (same instance handed to
    /// providers). Stored here because resolve_content needs direct GET access.
    http_client: reqwest::Client,
    /// Specialist extractor router. Empty in Phase 1. Behind `Arc` so
    /// `SearchService: Clone` still holds (the router is not `Clone`).
    source_router: Arc<crate::sources::SourceRouter>,
}

/// The credential-derived half of a [`SearchService`]: everything that must be
/// rebuilt when the caller's keys change. [`build_providers`] constructs these
/// from a [`Config`]; [`SearchService::new`] uses it with a process-wide config
/// (the stdio path), and [`SearchService::with_config`] uses it per request
/// while sharing the long-lived HTTP client and source cache.
struct ProviderSet {
    ai: Option<Arc<dyn AiProvider>>,
    ai_unavailable: Option<String>,
    default_model: String,
    source_slots: Vec<SourceSlot>,
    source_router: Arc<crate::sources::SourceRouter>,
}

/// Build the credential-bearing providers for a given `config`, reusing the
/// caller-supplied shared `http` client. Extracted verbatim from the original
/// `SearchService::new` body so both the process-wide (stdio) and per-request
/// (HTTP) construction paths share one implementation.
fn build_providers(config: &Config, http: &reqwest::Client) -> Result<ProviderSet> {
    build_providers_with_grok(config, http, http)
}

/// Like [`build_providers`], but the Grok **Responses** provider uses
/// `grok_http` while every other provider (Tavily / Firecrawl / source
/// fetching) keeps `http`. The HTTP transport passes a DNS-pinned,
/// no-redirect client as `grok_http` for a caller-supplied gateway
/// (`X-Grok-Base-Url`) — that restriction must apply to the gateway request
/// only, not to unrelated fetch/search traffic.
fn build_providers_with_grok(
    config: &Config,
    http: &reqwest::Client,
    grok_http: &reqwest::Client,
) -> Result<ProviderSet> {
    // Grok mode cannot run without its answer provider, so a missing key
    // fails construction exactly as before. Router mode never calls it: a
    // missing or broken Grok config is recorded for `doctor` instead.
    let (ai, ai_unavailable) = match build_ai_provider(config, http, grok_http) {
        Ok(ai) => (Some(ai), None),
        Err(err) if config.search_engine == SearchEngine::Grok => return Err(err),
        Err(err) => (None, Some(err.to_string())),
    };

    let source_slots = build_source_slots(config, http)?;

    let source_router = Arc::new(crate::sources::SourceRouter::from_config(config));

    Ok(ProviderSet {
        ai,
        ai_unavailable,
        default_model: resolve_default_model(config),
        source_slots,
        source_router,
    })
}

/// Build the Grok Responses or OpenAI-compatible answer provider from
/// `config`. Errors name the missing setting.
fn build_ai_provider(
    config: &Config,
    http: &reqwest::Client,
    grok_http: &reqwest::Client,
) -> Result<Arc<dyn AiProvider>> {
    use crate::config::Transport;

    let ai: Arc<dyn AiProvider> = match config.transport {
        Transport::Responses => {
            let credential: Arc<dyn crate::credentials::CredentialProvider> =
                match config.grok_auth_mode {
                    AuthMode::ApiKey => Arc::new(StaticApiKeyCredential::new(
                        config
                            .grok_api_key
                            .clone()
                            .ok_or(GrokSearchError::MissingConfig("GROK_SEARCH_API_KEY"))?,
                    )),
                    AuthMode::OAuth => {
                        let auth_path = config
                            .grok_auth_file
                            .clone()
                            .or_else(crate::config::auth_path)
                            .ok_or_else(|| {
                                GrokSearchError::OAuth(
                                    "oauth_auth_path_unavailable: set GROK_SEARCH_AUTH_FILE"
                                        .to_string(),
                                )
                            })?;
                        Arc::new(OAuthCredential::new(http.clone(), auth_path))
                    }
                };
            Arc::new(GrokResponsesProvider::with_credential_client(
                grok_http.clone(),
                config.grok_api_url.clone(),
                credential,
                config.web_search_enabled,
                config.x_search_enabled,
            ))
        }
        Transport::ChatCompletions => {
            let key = config
                .grok_api_key
                .clone()
                .ok_or(GrokSearchError::MissingConfig("GROK_SEARCH_API_KEY"))?;
            if config.x_search_enabled {
                eprintln!(
                    "llm-search: x_search_enabled is ignored when GROK_SEARCH_PROTOCOL=chat_completions"
                );
            }
            // `grok_http`, not `http`: GROK_SEARCH_URL may be a caller-supplied
            // gateway (X-Grok-Base-Url) on the HTTP transport, so this request
            // needs the same DNS-pinned, no-redirect client as Responses.
            Arc::new(
                crate::providers::openai_compatible::OpenAICompatProvider::with_client(
                    grok_http.clone(),
                    config.grok_api_url.clone(),
                    key,
                    config.grok_model.clone(),
                    config.web_search_enabled,
                ),
            )
        }
    };
    Ok(ai)
}

/// Non-reversible per-tenant namespace tag derived from the caller's primary
/// key. Cache entries are stored under `tag:session_id` so one tenant can never
/// read another tenant's cached `get_sources` pages on the shared HTTP process.
/// For stdio (a single process key) the tag is constant, so behavior is
/// unchanged. The gateway URL is part of the hash material: with arbitrary
/// public gateways two tenants on different gateways may present the same
/// opaque key string, and they must not share a cache namespace. Uses a
/// SHA-256 prefix — never any fragment of the raw key.
fn tenant_tag(config: &Config) -> String {
    let key = config.grok_api_key.as_deref().unwrap_or("");
    if key.is_empty() {
        return "anon".to_string();
    }
    let material = format!("{}\n{}", config.grok_api_url, key);
    let digest = ring::digest::digest(&ring::digest::SHA256, material.as_bytes());
    digest.as_ref()[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

impl SearchService {
    pub fn new(config: Config) -> Result<Self> {
        let http = crate::providers::http::build_client(config.timeout);
        let providers = build_providers(&config, &http)?;
        let cache = Arc::new(Mutex::new(SourceCache::new(config.cache_size)));
        Ok(Self::from_parts(config, http, cache, providers))
    }

    /// Build a request-scoped service that reuses this service's shared HTTP
    /// client and source cache, but derives every credential-bearing provider
    /// from `config`. The HTTP transport calls this per request so each caller
    /// searches with their own keys while the process keeps a single source
    /// cache — so `get_sources` continuation still works across requests.
    ///
    /// OAuth is rejected here: it resolves a single on-disk identity and is
    /// incompatible with per-request, multi-tenant credentials. HTTP callers
    /// must pass an API key. The local stdio path keeps OAuth via [`new`].
    ///
    /// [`new`]: SearchService::new
    pub fn with_config(&self, config: Config) -> Result<Self> {
        Self::for_request(self.http_client.clone(), self.cache.clone(), config)
    }

    /// Build a request-scoped service from shared state — a reused HTTP client
    /// and the process-wide source cache — plus a per-request `config`. This is
    /// the entrypoint the HTTP transport uses: the server process holds no
    /// credentials of its own, so it keeps only the shared client + cache and
    /// constructs a fully-credentialed service per request from the caller's
    /// header keys. OAuth is rejected here (single on-disk identity is
    /// incompatible with per-request multi-tenancy); a missing required key
    /// fails at construction (fail-closed) rather than reusing any server key.
    pub fn for_request(
        http_client: reqwest::Client,
        cache: Arc<Mutex<SourceCache>>,
        config: Config,
    ) -> Result<Self> {
        Self::for_request_with_grok_client(http_client.clone(), http_client, cache, config)
    }

    /// Like [`for_request`], but the Grok provider uses `grok_client` while all
    /// other providers keep `http_client`. The HTTP transport passes a
    /// DNS-pinned, no-redirect client here for a caller-supplied gateway, so
    /// the pin/no-redirect restriction stays scoped to the gateway request and
    /// never degrades unrelated fetch/search redirect handling.
    ///
    /// [`for_request`]: SearchService::for_request
    pub fn for_request_with_grok_client(
        http_client: reqwest::Client,
        grok_client: reqwest::Client,
        cache: Arc<Mutex<SourceCache>>,
        config: Config,
    ) -> Result<Self> {
        if config.grok_auth_mode == AuthMode::OAuth {
            return Err(GrokSearchError::OAuth(
                "oauth is not supported on the HTTP transport; pass a per-request API key"
                    .to_string(),
            ));
        }
        let providers = build_providers_with_grok(&config, &http_client, &grok_client)?;
        Ok(Self::from_parts(config, http_client, cache, providers))
    }

    /// Assemble a `SearchService` from an already-built provider set plus the
    /// shared `http` client and `cache`. Single assembly point for both `new`
    /// (fresh client + cache) and `with_config` (shared client + cache).
    fn from_parts(
        config: Config,
        http: reqwest::Client,
        cache: Arc<Mutex<SourceCache>>,
        providers: ProviderSet,
    ) -> Self {
        Self {
            cache,
            default_model: providers.default_model,
            config,
            ai: providers.ai,
            ai_unavailable: providers.ai_unavailable,
            source_slots: Arc::new(providers.source_slots),
            http_client: http,
            source_router: providers.source_router,
        }
    }

    /// Every configured secret value, for redaction at the MCP boundary.
    pub fn secret_values(&self) -> Vec<String> {
        self.config.secret_values()
    }

    /// Instantiated providers from the chain, in chain order.
    fn active_sources(&self) -> Vec<SourceEntry> {
        self.source_slots
            .iter()
            .filter_map(|slot| match slot {
                SourceSlot::Active(entry) => Some(entry.clone()),
                SourceSlot::Missing { .. } => None,
            })
            .collect()
    }

    /// Namespace a session id with the caller's tenant tag so cached
    /// `get_sources` pages are isolated per tenant. The plain `session_id`
    /// returned to the caller is unchanged; only the internal cache key is
    /// prefixed.
    fn tenant_cache_key(&self, session_id: &str) -> String {
        format!("{}:{}", tenant_tag(&self.config), session_id)
    }

    pub fn fake_with_sources() -> Self {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "fake-grok"),
            ("TAVILY_API_KEY", "fake-tavily"),
        ]);
        let firecrawl_enabled = config.firecrawl_enabled;
        Self {
            cache: Arc::new(Mutex::new(SourceCache::new(256))),
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(vec![
                SourceSlot::Active(SourceEntry {
                    spec: &TAVILY_SPEC,
                    provider: Arc::new(FakeSourceProvider),
                }),
                SourceSlot::Missing {
                    spec: &FIRECRAWL_SPEC,
                    enabled: firecrawl_enabled,
                },
            ]),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        }
    }

    /// Legacy two-slot wiring for test factories: `primary` occupies the
    /// Tavily slot and `fallback` the Firecrawl slot, exactly as the
    /// pre-chain service was shaped.
    fn fake_slots(
        primary: Arc<dyn SourceProvider>,
        fallback: Option<Arc<dyn SourceProvider>>,
        firecrawl_enabled: bool,
    ) -> Arc<Vec<SourceSlot>> {
        let firecrawl_slot = match fallback {
            Some(provider) => SourceSlot::Active(SourceEntry {
                spec: &FIRECRAWL_SPEC,
                provider,
            }),
            None => SourceSlot::Missing {
                spec: &FIRECRAWL_SPEC,
                enabled: firecrawl_enabled,
            },
        };
        Arc::new(vec![
            SourceSlot::Active(SourceEntry {
                spec: &TAVILY_SPEC,
                provider: primary,
            }),
            firecrawl_slot,
        ])
    }

    /// Unified test factory: override AI / primary / fallback providers and
    /// inject extra env vars. Use `fake_with_sources()` for the trivial case.
    pub fn fake_custom<I, K, V>(
        ai: Option<Arc<dyn AiProvider>>,
        primary: Arc<dyn SourceProvider>,
        fallback: Option<Arc<dyn SourceProvider>>,
        overrides: I,
    ) -> Self
    where
        I: IntoIterator<Item = (K, V)>,
        K: Into<String>,
        V: Into<String>,
    {
        let mut vars = vec![
            ("LLM_SEARCH_PROVIDER".to_string(), "grok".to_string()),
            ("GROK_SEARCH_API_KEY".to_string(), "fake-grok".to_string()),
            ("TAVILY_API_KEY".to_string(), "fake-tavily".to_string()),
        ];
        if fallback.is_some() {
            vars.push((
                "FIRECRAWL_API_KEY".to_string(),
                "fake-firecrawl".to_string(),
            ));
        }
        vars.extend(
            overrides
                .into_iter()
                .map(|(key, value)| (key.into(), value.into())),
        );
        let config = Config::from_env_map(vars);

        let source_slots = Self::fake_slots(primary, fallback, config.firecrawl_enabled);
        Self {
            cache: Arc::new(Mutex::new(SourceCache::new(256))),
            default_model: resolve_default_model(&config),
            config,
            ai: Some(ai.unwrap_or_else(|| Arc::new(FakeAiProvider))),
            ai_unavailable: None,
            source_slots,
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        }
    }

    /// Test factory that injects a populated [`crate::sources::SourceRouter`] so
    /// fallback behavior can be exercised with fake extractors. Mirrors
    /// `fake_custom`'s provider wiring.
    pub fn fake_with_router(
        primary: Arc<dyn SourceProvider>,
        fallback: Option<Arc<dyn SourceProvider>>,
        router: crate::sources::SourceRouter,
    ) -> Self {
        let mut vars = vec![
            ("LLM_SEARCH_PROVIDER".to_string(), "grok".to_string()),
            ("GROK_SEARCH_API_KEY".to_string(), "fake-grok".to_string()),
            ("TAVILY_API_KEY".to_string(), "fake-tavily".to_string()),
        ];
        if fallback.is_some() {
            vars.push((
                "FIRECRAWL_API_KEY".to_string(),
                "fake-firecrawl".to_string(),
            ));
        }
        let config = Config::from_env_map(vars);
        let source_slots = Self::fake_slots(primary, fallback, config.firecrawl_enabled);
        Self {
            cache: Arc::new(Mutex::new(SourceCache::new(256))),
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots,
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(router),
        }
    }

    /// `web_search` entry point: the Brave / Tavily router by default, the
    /// original Grok pipeline when `LLM_SEARCH_PROVIDER=grok`.
    pub async fn web_search(&self, input: WebSearchInput) -> Result<WebSearchOutput> {
        // Research runs on Tavily whatever the configured engine.
        if input.mode.as_deref().map(str::trim) == Some("research") {
            return self.research_search(input).await;
        }
        match self.config.search_engine {
            SearchEngine::Brave => self.router_search(input).await,
            SearchEngine::Grok => {
                let mut warnings = Vec::new();
                if input.mode.is_some() {
                    warnings.push(
                        "mode is ignored when LLM_SEARCH_PROVIDER=grok (router modes need LLM_SEARCH_PROVIDER=brave)"
                            .to_string(),
                    );
                }
                let mut ignored: Vec<String> = input
                    .hints
                    .names()
                    .into_iter()
                    .map(str::to_string)
                    .collect();
                ignored.extend(input.tavily.keys().cloned());
                if !ignored.is_empty() {
                    warnings.push(format!(
                        "ignored when LLM_SEARCH_PROVIDER=grok: {}",
                        ignored.join(", ")
                    ));
                }
                if input.max_results.is_some() {
                    warnings.push(
                        "max_results is ignored when LLM_SEARCH_PROVIDER=grok (use extra_sources)"
                            .to_string(),
                    );
                }
                let query = input.query.clone();
                let mut output = self.grok_search(input).await?;
                output.source_log = build_source_log(&output.sources, Some(&query));
                output.warnings = warnings;
                Ok(output)
            }
        }
    }

    /// The original GrokSearch-rs pipeline: Grok answers, the source chain
    /// enriches or (when Grok is unusable) replaces it.
    async fn grok_search(&self, input: WebSearchInput) -> Result<WebSearchOutput> {
        let ai = self.ai.clone().ok_or_else(|| {
            GrokSearchError::Provider(format!(
                "LLM_SEARCH_PROVIDER=grok but the Grok provider is unavailable: {}",
                self.ai_unavailable
                    .as_deref()
                    .unwrap_or("no Grok configuration")
            ))
        })?;
        // D-02: single global deadline shared by Grok + supplemental fetch + enrichment.
        let deadline = tokio::time::Instant::now() + self.config.timeout;
        // response_format (Anthropic tool-design guidance: concise|detailed)
        // wins over the legacy include_content flag when both are present.
        let format_include_content = match input.response_format.as_deref() {
            None => None,
            Some("concise") => Some(false),
            Some("detailed") => Some(true),
            Some(other) => {
                return Err(GrokSearchError::InvalidParams(format!(
                    "response_format must be \"concise\" or \"detailed\", got \"{other}\""
                )))
            }
        };
        let include_content =
            format_include_content.unwrap_or_else(|| input.include_content.unwrap_or(true));

        let mut uuid_buf = [0u8; uuid::fmt::Simple::LENGTH];
        let session_id = {
            let encoded = Uuid::new_v4().simple().encode_lower(&mut uuid_buf);
            encoded[..12].to_string()
        };
        let effective_extra_sources = input
            .extra_sources
            .unwrap_or(self.config.default_extra_sources);

        let filters = search_filters(&input);

        // Speculative fan-out: fetch enough sources to satisfy whichever path
        // (enrichment or fallback) the Grok response routes us into. The
        // speculative call fires concurrently with Grok via tokio::join!, so
        // total latency is roughly max(Grok, Tavily) instead of the sum. The
        // single source call is then sliced to either `effective_extra_sources`
        // (enrichment) or `self.config.fallback_sources` (fallback), preserving
        // the legacy "exactly one source provider call per web_search" contract.
        let speculative_count = effective_extra_sources.max(self.config.fallback_sources);
        let request = self.build_search_request(&input, &[]);

        let grok_future = ai.search(&request);
        let speculative_future =
            self.fetch_raw_extra_sources(&input.query, speculative_count, &filters, deadline);
        let (grok_result, raw) = tokio::join!(grok_future, speculative_future);

        let response = match grok_result {
            Ok(response) => response,
            Err(err) => {
                return self
                    .finalize_fallback(
                        deadline,
                        session_id,
                        SearchResponse {
                            content: String::new(),
                            sources: Vec::new(),
                        },
                        raw,
                        grok_error_reason(&err),
                        include_content,
                    )
                    .await;
            }
        };

        if let Some(reason) = grok_unverifiable_reason(&response) {
            return self
                .finalize_fallback(deadline, session_id, response, raw, reason, include_content)
                .await;
        }

        let mut enrichment = raw.sources;
        enrichment.truncate(effective_extra_sources);
        let enrichment = with_provider(enrichment, enrichment_label(raw.origin));
        let merged = merge_sources(response.sources, enrichment);
        // SRCH-04 dual gate (zero-regression): skip enrichment when the caller
        // opted out OR there are no supplemental sources. Gating on
        // include_content alone would leave content populated at extra_sources=0
        // and break the legacy "summary + source list" shape.
        let merged = if include_content && effective_extra_sources > 0 {
            enrich_sources(
                merged,
                deadline,
                &self.http_client,
                &self.source_router,
                crate::sources::SourceCaps {
                    max_answers: self.config.source_max_answers,
                    max_comments: self.config.source_max_comments,
                },
                self.config.enrich_concurrency,
                self.config.enrich_max_chars,
                self.config.max_inline_sources,
                self.active_sources(),
            )
            .await
        } else {
            merged
        };

        let merged_arc = Arc::new(merged);
        let sources_count = merged_arc.len();
        let cache_key = self.tenant_cache_key(&session_id);
        self.cache.lock().await.set(cache_key, merged_arc.clone());

        // The cache keeps the full enriched content; only the returned copy is
        // trimmed to the response budget so drill-down loses nothing.
        let mut out_sources = (*merged_arc).clone();
        let truncated = apply_response_budget(
            response.content.chars().count(),
            &mut out_sources,
            self.config.response_max_chars,
            &session_id,
        );

        Ok(WebSearchOutput {
            session_id,
            content: response.content,
            sources_count,
            sources: out_sources,
            search_provider: "grok_responses".to_string(),
            fallback_used: false,
            fallback_reason: None,
            truncated,
            engine: SearchEngine::Grok.as_str().to_string(),
            mode: None,
            answer: None,
            fallbacks: Vec::new(),
            notice: None,
            source_log: Vec::new(),
            warnings: Vec::new(),
            request_id: None,
            usage: None,
            images: Vec::new(),
            ..WebSearchOutput::default()
        })
    }

    /// Brave / Tavily router (`LLM_SEARCH_PROVIDER=brave`), after the search
    /// router of deepseek-harness-web-search-plugin:
    ///
    /// - `quick` (default): Brave, then Tavily, then the rest of the chain.
    /// - `deep`: Tavily (advanced depth, provider answer, inline page text),
    ///   then Brave, then the rest of the chain.
    /// - `verify`: Brave and Tavily in parallel, merged by URL; the rest of
    ///   the chain only when both fail.
    ///
    /// Every provider tried before the one that served the result is listed
    /// in `fallbacks`, and `notice` asks the caller to disclose it.
    async fn router_search(&self, input: WebSearchInput) -> Result<WebSearchOutput> {
        let deadline = tokio::time::Instant::now() + self.config.timeout;
        let mode = RouterMode::parse(input.mode.as_deref())?;
        let query = input.query.trim().to_string();
        if query.is_empty() {
            return Err(GrokSearchError::InvalidParams(
                "web_search.query is required".to_string(),
            ));
        }
        let max_results = input
            .max_results
            .unwrap_or(ROUTER_DEFAULT_RESULTS)
            .clamp(1, ROUTER_MAX_RESULTS);
        // Router results are snippets (or Tavily page text in deep/verify);
        // fetching pages for them is opt-in, unlike Grok mode.
        let include_content = match input.response_format.as_deref() {
            None => input.include_content.unwrap_or(false),
            Some("concise") => false,
            Some("detailed") => true,
            Some(other) => {
                return Err(GrokSearchError::InvalidParams(format!(
                    "response_format must be \"concise\" or \"detailed\", got \"{other}\""
                )))
            }
        };
        let filters = search_filters(&input);
        let mut warnings = Vec::new();
        if input.extra_sources.is_some() {
            warnings.push(
                "extra_sources is ignored when LLM_SEARCH_PROVIDER=brave (use max_results)"
                    .to_string(),
            );
        }

        let active = self.active_sources();
        let find = |name: &str| active.iter().find(|entry| entry.spec.name == name).cloned();
        let brave = find("brave");
        let tavily = find("tavily");
        let rest: Vec<SourceEntry> = active
            .iter()
            .filter(|entry| !matches!(entry.spec.name, "brave" | "tavily"))
            .cloned()
            .collect();
        if brave.is_none() && tavily.is_none() && rest.is_empty() {
            return Err(GrokSearchError::MissingConfig(
                "BRAVE_API_KEY, TAVILY_API_KEY, EXA_API_KEY, TINYFISH_API_KEY or FIRECRAWL_API_KEY (web_search has no search provider configured)",
            ));
        }

        let call = RouterCall {
            query: &query,
            max_results,
            filters: &filters,
            mode,
            deadline,
            hints: &input.hints,
            tavily: &input.tavily,
        };
        let mut fallbacks: Vec<SearchFallback> = Vec::new();
        let served = match mode {
            RouterMode::Quick | RouterMode::Deep => {
                let names: Vec<&'static str> = active.iter().map(|entry| entry.spec.name).collect();
                let order: Vec<SourceEntry> = router_mode_order(&names, mode, &self.config)
                    .into_iter()
                    .filter_map(find)
                    .collect();
                router_first_hit(&order, &call, &mut fallbacks).await
            }
            RouterMode::Verify => {
                let (brave_attempt, tavily_attempt) = tokio::join!(
                    router_attempt_opt(brave.as_ref(), &call),
                    router_attempt_opt(tavily.as_ref(), &call),
                );
                let mut successes: Vec<(&'static str, DetailedSearch)> = Vec::new();
                for attempt in [brave_attempt, tavily_attempt].into_iter().flatten() {
                    match attempt {
                        (name, Ok(found)) => successes.push((name, found)),
                        (name, Err(error)) => fallbacks.push(SearchFallback {
                            provider: name.to_string(),
                            error,
                        }),
                    }
                }
                if successes.is_empty() {
                    router_first_hit(&rest, &call, &mut fallbacks).await
                } else {
                    let label = if successes.len() > 1 {
                        "mixed".to_string()
                    } else {
                        successes[0].0.to_string()
                    };
                    let answer = successes
                        .iter()
                        .filter_map(|(_, found)| found.answer.clone())
                        .collect::<Vec<_>>()
                        .join("\n\n");
                    let mut merged: Vec<Source> = Vec::new();
                    let mut tavily_meta = DetailedSearch::default();
                    for (name, found) in successes {
                        if name == "tavily" {
                            tavily_meta.request_id = found.request_id;
                            tavily_meta.usage = found.usage;
                            tavily_meta.images = found.images;
                        }
                        merged = merge_sources(merged, found.sources);
                    }
                    merged.truncate(max_results);
                    Some((
                        label,
                        DetailedSearch {
                            answer: (!answer.trim().is_empty()).then_some(answer),
                            sources: merged,
                            ..tavily_meta
                        },
                    ))
                }
            }
        };

        let Some((provider_label, found)) = served else {
            let detail = fallbacks
                .iter()
                .map(|entry| format!("{}: {}", entry.provider, entry.error))
                .collect::<Vec<_>>()
                .join("; ");
            return Err(GrokSearchError::Provider(format!(
                "all search providers failed ({detail})"
            )));
        };

        let served_names: Vec<&str> = if provider_label == "mixed" {
            vec!["brave", "tavily"]
        } else {
            vec![provider_label.as_str()]
        };
        warnings.extend(ignored_param_warnings(
            &served_names,
            &input.hints,
            &input.tavily,
            &router_options_for("tavily", mode, &input.hints, &input.tavily),
        ));
        let DetailedSearch {
            answer,
            sources,
            request_id,
            usage,
            images,
        } = found;
        let sources = if include_content {
            self.enrich_missing_content(sources, deadline).await
        } else {
            sources
        };

        let mut uuid_buf = [0u8; uuid::fmt::Simple::LENGTH];
        let session_id = {
            let encoded = Uuid::new_v4().simple().encode_lower(&mut uuid_buf);
            encoded[..12].to_string()
        };
        let sources_arc = Arc::new(sources);
        let sources_count = sources_arc.len();
        let cache_key = self.tenant_cache_key(&session_id);
        self.cache.lock().await.set(cache_key, sources_arc.clone());

        let content =
            render_router_content(&provider_label, mode, sources_count, answer.as_deref());
        let mut out_sources = (*sources_arc).clone();
        let truncated = apply_response_budget(
            content.chars().count(),
            &mut out_sources,
            self.config.response_max_chars,
            &session_id,
        );
        let source_log = build_source_log(&out_sources, Some(&query));
        let notice = fallback_notice(&fallbacks);

        Ok(WebSearchOutput {
            session_id,
            content,
            sources_count,
            sources: out_sources,
            search_provider: provider_label,
            fallback_used: !fallbacks.is_empty(),
            fallback_reason: fallbacks.first().map(fallback_reason_id),
            truncated,
            engine: SearchEngine::Brave.as_str().to_string(),
            mode: Some(mode.as_str().to_string()),
            answer,
            fallbacks,
            notice,
            source_log,
            warnings,
            request_id,
            usage,
            images,
            ..WebSearchOutput::default()
        })
    }

    /// Inline page content for the sources that do not already carry it
    /// (Tavily deep results do), through the same specialist + source-chain
    /// pipeline as Grok-mode enrichment.
    async fn enrich_missing_content(
        &self,
        sources: Vec<Source>,
        deadline: tokio::time::Instant,
    ) -> Vec<Source> {
        let (missing_idx, missing): (Vec<usize>, Vec<Source>) = sources
            .iter()
            .enumerate()
            .filter(|(_, source)| source.content.is_none())
            .map(|(idx, source)| (idx, source.clone()))
            .unzip();
        if missing.is_empty() {
            return sources;
        }
        let enriched = enrich_sources(
            missing,
            deadline,
            &self.http_client,
            &self.source_router,
            crate::sources::SourceCaps {
                max_answers: self.config.source_max_answers,
                max_comments: self.config.source_max_comments,
            },
            self.config.enrich_concurrency,
            self.config.enrich_max_chars,
            self.config.max_inline_sources,
            self.active_sources(),
        )
        .await;
        let mut out = sources;
        for (idx, source) in missing_idx.into_iter().zip(enriched) {
            out[idx] = source;
        }
        out
    }

    /// Fetch sources by walking the configured provider chain in order; the
    /// first provider with usable results wins. No path-specific provider
    /// label is applied here — the returned Vec carries each provider's
    /// native label ("tavily"/"exa"/…); the caller re-labels via
    /// `with_provider` once the path (enrichment vs fallback) is known.
    ///
    /// Every path that yields nothing records *why* in [`RawSources::notes`].
    /// Provider errors used to be swallowed outright, which left a request that
    /// ended with zero sources indistinguishable from one where every upstream
    /// was simply out of results — the operator had no way to tell an unset key
    /// from a rate-limited one.
    async fn fetch_raw_extra_sources(
        &self,
        query: &str,
        count: usize,
        filters: &SearchFilters,
        deadline: tokio::time::Instant,
    ) -> RawSources {
        if count == 0 {
            return RawSources::empty(vec![SourceNote::config(
                "source fan-out disabled (extra_sources and fallback_sources are both 0)",
            )]);
        }
        let mut notes = Vec::new();
        for slot in self.source_slots.iter() {
            match slot {
                SourceSlot::Active(entry) => {
                    let name = entry.spec.name;
                    // A provider that ignores `SearchFilters` (Firecrawl) must
                    // never serve a filter-constrained request: swapping
                    // constrained results for unconstrained ones would
                    // silently violate the include/exclude/recency contract.
                    // Filtered requests are filter-capable-providers-or-nothing.
                    if !filters_honored(entry.spec, filters) {
                        notes.push(SourceNote::no_results(format!(
                            "{name}: skipped (request carries domain/recency filters, which {name} cannot honor)"
                        )));
                        continue;
                    }
                    // D-02: the whole chain shares the request's global
                    // deadline. Without this, every chain position gets a
                    // fresh full client timeout and a slow chain multiplies
                    // the configured budget by its length.
                    let attempt = tokio::time::timeout_at(
                        deadline,
                        entry.provider.search_sources(query, count, filters),
                    )
                    .await;
                    let outcome = match attempt {
                        Ok(outcome) => outcome,
                        Err(_elapsed) => {
                            // Later providers would hit the same expired
                            // deadline immediately; one note explains the stop.
                            notes.push(SourceNote::broken(format!(
                                "{name}: timed out (request deadline reached; providers after it were not tried)"
                            )));
                            break;
                        }
                    };
                    match outcome {
                        Ok(sources) => match usable_or_note(name, sources) {
                            Ok(usable) => {
                                if !notes.is_empty() {
                                    eprintln!(
                                        "llm-search: source chain fell through to {name} ({})",
                                        source_diagnosis(&notes)
                                    );
                                }
                                return RawSources::found(usable, Some(name));
                            }
                            Err(note) => notes.push(note),
                        },
                        Err(err) => notes.push(SourceNote::broken(format!("{name}: {err}"))),
                    }
                }
                SourceSlot::Missing { spec, enabled } => {
                    let unavailable = unavailable_note(
                        spec.name,
                        *enabled,
                        spec.enable_var,
                        spec.key_var,
                        spec.header,
                    );
                    // For a filter-blind provider, whether the gate is the
                    // *only* thing in the way decides what to advise. A
                    // configured Firecrawl really would have answered, so
                    // dropping the filters is a genuine remedy; an absent one
                    // changes nothing when the filters go, so the note names
                    // both problems instead of hiding the config one.
                    notes.push(if !filters_honored(spec, filters) {
                        SourceNote::new(
                            unavailable.kind,
                            format!(
                                "{} — also skipped (request carries domain/recency filters, which {} cannot honor)",
                                unavailable.detail, spec.name
                            ),
                        )
                    } else {
                        unavailable
                    });
                }
            }
        }
        RawSources::empty(notes)
    }

    #[allow(clippy::too_many_arguments)]
    async fn finalize_fallback(
        &self,
        deadline: tokio::time::Instant,
        session_id: String,
        response: SearchResponse,
        raw: RawSources,
        reason: &str,
        include_content: bool,
    ) -> Result<WebSearchOutput> {
        let RawSources {
            sources,
            origin,
            notes,
        } = raw;
        let mut fallback = sources;
        fallback.truncate(self.config.fallback_sources);
        let fallback = with_provider(fallback, fallback_label(origin));
        // A metadata-only Grok response — citations but no prose — routes here
        // as `grok_content_empty`, and its citations are real evidence. Merge
        // them rather than dropping them: they keep their `grok_responses`
        // label, exactly as on the success path.
        let fallback = merge_sources(response.sources, fallback);

        // A degraded response with zero sources carries no evidence *and* no
        // answer — the original Grok text is deliberately not echoed on this
        // path. Returning `Ok` here handed the caller a successful-looking
        // empty result, which client models read as "the tool works, this
        // query just missed" and answer by retrying the same dead path
        // forever. Fail loudly instead, naming the upstream reason, what each
        // source provider did, and what would actually change the outcome.
        if fallback.is_empty() {
            // A zero budget truncates the fan-out to nothing whatever the
            // providers returned, so it outranks every other account of the
            // failure: no reformulated query can rescue this path while it
            // holds. Testing the config directly rather than inferring it from
            // an empty source list is what keeps that true when the providers
            // also came back empty.
            return Err(GrokSearchError::Provider(
                if self.config.fallback_sources == 0 {
                    // Providers that were consulted still get to say what they
                    // saw; when the budget alone is at fault there are no notes
                    // and nothing to append.
                    let detail = if notes.is_empty() {
                        String::new()
                    } else {
                        format!(" ({})", source_diagnosis(&notes))
                    };
                    format!(
                        "web_search returned no sources: {reason}, and the fallback source budget is 0 (GROK_SEARCH_FALLBACK_SOURCES), so nothing from the source fan-out can survive{detail}. Raise that budget to get evidence on this path; no change of query can help while it is 0."
                    )
                } else {
                    format!(
                        "web_search returned no sources: {reason}, and the source fallback produced nothing ({}). {}",
                        source_diagnosis(&notes),
                        fallback_remediation(&notes)
                    )
                },
            ));
        }

        // D-03: the degraded path enriches eagerly — one-hand evidence is most
        // valuable when there is no verifiable summary, so there is no
        // extra_sources gate here (that gate is the normal web_search path's
        // concern, SRCH-04). The one exception is an explicit include_content=false
        // opt-out, which must be honored everywhere so callers who disabled inline
        // content never pay the extra fetch budget.
        let fallback = if include_content {
            enrich_sources(
                fallback,
                deadline,
                &self.http_client,
                &self.source_router,
                crate::sources::SourceCaps {
                    max_answers: self.config.source_max_answers,
                    max_comments: self.config.source_max_comments,
                },
                self.config.enrich_concurrency,
                self.config.enrich_max_chars,
                self.config.max_inline_sources,
                self.active_sources(),
            )
            .await
        } else {
            fallback
        };

        let fallback_arc = Arc::new(fallback);
        let sources_count = fallback_arc.len();
        let cache_key = self.tenant_cache_key(&session_id);
        self.cache.lock().await.set(cache_key, fallback_arc.clone());

        let content = if response.content.trim().is_empty() {
            format!(
                "Grok Responses search did not return a verifiable answer. Source fallback returned {sources_count} source(s); evaluate them directly rather than treating any text as a verified answer."
            )
        } else {
            format!(
                "Grok Responses returned an answer without verifiable search sources, so source fallback returned {sources_count} source(s). Original Grok answer was not treated as verified; evaluate the listed sources directly."
            )
        };

        let mut out_sources = (*fallback_arc).clone();
        let truncated = apply_response_budget(
            content.chars().count(),
            &mut out_sources,
            self.config.response_max_chars,
            &session_id,
        );

        let serving = origin.unwrap_or("source chain");
        Ok(WebSearchOutput {
            session_id,
            content,
            sources_count,
            sources: out_sources,
            search_provider: "source_fallback".to_string(),
            fallback_used: true,
            fallback_reason: Some(reason.to_string()),
            truncated,
            engine: SearchEngine::Grok.as_str().to_string(),
            mode: None,
            answer: None,
            fallbacks: vec![SearchFallback {
                provider: "grok".to_string(),
                error: reason.to_string(),
            }],
            notice: Some(format!(
                "Search provider fallback used: grok: {reason}; results come from {serving}. Disclose this if the answer relies on fallback results."
            )),
            source_log: Vec::new(),
            warnings: Vec::new(),
            request_id: None,
            usage: None,
            images: Vec::new(),
            ..WebSearchOutput::default()
        })
    }

    /// Return one page of cached sources for a prior `web_search` session.
    /// `offset`/`limit` follow the official MCP fetch server's `start_index`
    /// continuation pattern, applied to sources; an offset past the end is an
    /// empty page, not an error. Each page is additionally subject to the
    /// response budget (`truncated` reports in-page trimming).
    pub async fn get_sources(
        &self,
        session_id: &str,
        offset: usize,
        limit: Option<usize>,
    ) -> Result<GetSourcesOutput> {
        let cached = self
            .cache
            .lock()
            .await
            .get(&self.tenant_cache_key(session_id))
            .ok_or_else(|| GrokSearchError::NotFound(format!("session_id={session_id}")))?;
        let total_sources = cached.len();
        let start = offset.min(total_sources);
        let end = limit
            .map_or(total_sources, |l| start.saturating_add(l))
            .min(total_sources);
        let mut page: Vec<Source> = cached[start..end].to_vec();
        let truncated =
            apply_response_budget(0, &mut page, self.config.response_max_chars, session_id);
        // Budget trimming may shorten the page; continue from what was
        // actually returned, not from the requested slice end.
        let served_end = start + page.len();
        Ok(GetSourcesOutput {
            session_id: session_id.to_string(),
            sources_count: page.len(),
            sources: page,
            total_sources,
            offset,
            next_offset: (served_end < total_sources).then_some(served_end),
            truncated,
        })
    }

    pub async fn web_fetch(&self, url: &str, max_chars: Option<usize>) -> Result<WebFetchOutput> {
        self.web_fetch_with(url, max_chars, &FetchOptions::default())
            .await
    }

    /// Single-URL `web_fetch` with extraction options. `options` reach only
    /// providers that honor them (Tavily extract fields); a specialist
    /// extractor (GitHub, StackExchange, arXiv, Wikipedia) ignores them.
    pub async fn web_fetch_with(
        &self,
        url: &str,
        max_chars: Option<usize>,
        options: &FetchOptions,
    ) -> Result<WebFetchOutput> {
        let effective_limit = max_chars.or(self.config.fetch_max_chars);
        // D-02, as on the web_search path: one deadline for the whole call.
        // The specialist attempt and every generic-chain provider draw from
        // the same budget, so a slow specialist followed by a slow chain
        // cannot spend a full LLM_SEARCH_TIMEOUT_SECONDS apiece.
        let deadline = tokio::time::Instant::now() + self.fetch_budget(options);
        let (page, source_type, fallback_reason, provider) =
            match self.specialist_stage(url, deadline).await {
                SpecialistStage::Served(content, kind) => {
                    let label = source_type_label(&kind);
                    (FetchedPage::text(content), kind, None, label)
                }
                // No specialist matched (reason None, D-01) or it matched and
                // failed (reason surfaced): go generic.
                SpecialistStage::Generic(reason) => {
                    let (page, served) = self.web_fetch_raw(url, deadline, options).await?;
                    (
                        page,
                        crate::sources::SourceType::Generic,
                        reason,
                        Some(served.to_string()),
                    )
                }
                // The budget is gone, so the generic chain cannot run either.
                SpecialistStage::TimedOut => return Err(fetch_timeout(url)),
            };
        Ok(fetch_output(
            url,
            page,
            effective_limit,
            source_type,
            fallback_reason,
            provider,
        ))
    }

    /// Deadline budget for a fetch: the configured timeout, stretched to fit
    /// a caller-set Tavily extract `timeout` plus a round-trip margin.
    fn fetch_budget(&self, options: &FetchOptions) -> std::time::Duration {
        let base = self.config.timeout;
        options
            .tavily_timeout()
            .map(|limit| (limit + std::time::Duration::from_secs(10)).max(base))
            .unwrap_or(base)
    }

    /// Specialist stage of one fetch, bounded by `deadline`.
    async fn specialist_stage(&self, url: &str, deadline: tokio::time::Instant) -> SpecialistStage {
        // Malformed URL is not a specialist failure — go generic, no reason.
        let Ok(parsed) = url::Url::parse(url) else {
            return SpecialistStage::Generic(None);
        };
        let caps = crate::sources::SourceCaps {
            max_answers: self.config.source_max_answers,
            max_comments: self.config.source_max_comments,
        };
        let specialist = crate::sources::resolve_content(
            &self.http_client,
            &parsed,
            self.source_router.as_ref(),
            &caps,
        );
        match tokio::time::timeout_at(deadline, specialist).await {
            Ok(Ok((content, kind))) => SpecialistStage::Served(content, kind),
            Ok(Err(reason)) if reason == crate::sources::NO_SPECIALIST_MATCH => {
                SpecialistStage::Generic(None)
            }
            Ok(Err(reason)) => SpecialistStage::Generic(Some(reason)),
            Err(_elapsed) => SpecialistStage::TimedOut,
        }
    }

    /// Fetch several URLs (the `urls` argument). A failed URL is reported in
    /// `failed` instead of failing the call. `max_chars` is one budget shared
    /// by all results, spent in input order; it defaults to the response
    /// budget (LLM_SEARCH_RESPONSE_MAX_CHARS).
    ///
    /// Specialists run per URL first. Every URL left for the generic chain
    /// goes to the first fetch-capable provider in one batch call when it
    /// supports batching (Tavily); URLs it cannot read continue down the rest
    /// of the chain one by one. A chain whose first fetch provider cannot
    /// batch runs the single-URL pipeline per URL, as before.
    pub async fn web_fetch_batch(
        &self,
        urls: &[String],
        max_chars: Option<usize>,
        options: &FetchOptions,
    ) -> Result<WebFetchBatchOutput> {
        let (outcomes, request_ids, usage) = if urls.len() > 1 {
            self.fetch_via_batch(urls, options).await
        } else {
            (self.fetch_each(urls, options).await, Vec::new(), None)
        };

        let mut remaining = max_chars.unwrap_or(self.config.response_max_chars);
        let mut results = Vec::new();
        let mut failed = Vec::new();
        let mut source_log = Vec::new();
        let mut truncated = false;
        for (idx, outcome) in outcomes.into_iter().enumerate() {
            let url = &urls[idx];
            let source_id = format!("src_{:03}", idx + 1);
            match outcome {
                Ok(page) => {
                    let mut page = if page.content.chars().count() > remaining {
                        truncated = true;
                        let extracted_at = page.extracted_at.clone();
                        let provider = page.provider.clone();
                        let extras = page.extras.clone();
                        let cut = apply_fetch_limit(
                            &page.url,
                            page.content,
                            Some(remaining),
                            page.source_type,
                            page.fallback_reason,
                        );
                        let mut cut = finish_fetch_output(cut, provider, extracted_at);
                        cut.extras = extras;
                        cut
                    } else {
                        page
                    };
                    remaining = remaining.saturating_sub(page.content.chars().count());
                    for entry in &mut page.source_log {
                        entry.source_id = source_id.clone();
                    }
                    source_log.append(&mut page.source_log);
                    results.push(page);
                }
                Err(err) => {
                    let error = crate::model::source::truncate_with_marker(
                        &err.to_string(),
                        FALLBACK_ERROR_CHARS,
                    );
                    source_log.push(SourceLogEntry {
                        source_id,
                        url: url.clone(),
                        domain: domain_of(url),
                        status: "failed".to_string(),
                        ..SourceLogEntry::default()
                    });
                    failed.push(FetchFailure {
                        url: url.clone(),
                        error,
                    });
                }
            }
        }
        Ok(WebFetchBatchOutput {
            results,
            failed,
            source_log,
            truncated,
            request_ids,
            usage,
        })
    }

    /// Single-URL pipeline for each URL concurrently; outcomes in input order.
    async fn fetch_each(
        &self,
        urls: &[String],
        options: &FetchOptions,
    ) -> Vec<Result<WebFetchOutput>> {
        let sem = Arc::new(tokio::sync::Semaphore::new(
            self.config.enrich_concurrency.max(1),
        ));
        let mut set: tokio::task::JoinSet<(usize, Result<WebFetchOutput>)> =
            tokio::task::JoinSet::new();
        for (idx, url) in urls.iter().enumerate() {
            let service = self.clone();
            let url = url.clone();
            let options = options.clone();
            let permit = Arc::clone(&sem);
            set.spawn(async move {
                let _permit = permit.acquire_owned().await.ok();
                (idx, service.web_fetch_with(&url, None, &options).await)
            });
        }
        collect_in_order(set, urls).await
    }

    /// The batch pipeline described on [`web_fetch_batch`](Self::web_fetch_batch).
    /// Returns outcomes in input order plus the batch call's request ids and
    /// credit usage.
    async fn fetch_via_batch(
        &self,
        urls: &[String],
        options: &FetchOptions,
    ) -> (
        Vec<Result<WebFetchOutput>>,
        Vec<String>,
        Option<serde_json::Value>,
    ) {
        // One batch call stands in for up to 20 per-URL fetches that each
        // had the full budget, and may be followed by an advanced retry: give
        // the batch path two budgets.
        let deadline = tokio::time::Instant::now() + self.fetch_budget(options) * 2;
        let limit = self.config.fetch_max_chars;
        let chain = self.active_sources();

        // 1. Specialists, concurrently.
        let mut set: tokio::task::JoinSet<(usize, SpecialistStage)> = tokio::task::JoinSet::new();
        for (idx, url) in urls.iter().enumerate() {
            let service = self.clone();
            let url = url.clone();
            set.spawn(async move { (idx, service.specialist_stage(&url, deadline).await) });
        }
        let mut stages: Vec<SpecialistStage> =
            (0..urls.len()).map(|_| SpecialistStage::TimedOut).collect();
        while let Some(joined) = set.join_next().await {
            if let Ok((idx, stage)) = joined {
                stages[idx] = stage;
            }
        }
        let mut outputs: Vec<Option<Result<WebFetchOutput>>> =
            (0..urls.len()).map(|_| None).collect();
        let mut generic: Vec<(usize, Option<String>)> = Vec::new();
        for (idx, stage) in stages.into_iter().enumerate() {
            match stage {
                SpecialistStage::Served(content, kind) => {
                    let label = source_type_label(&kind);
                    outputs[idx] = Some(Ok(fetch_output(
                        &urls[idx],
                        FetchedPage::text(content),
                        limit,
                        kind,
                        None,
                        label,
                    )));
                }
                SpecialistStage::Generic(reason) => generic.push((idx, reason)),
                SpecialistStage::TimedOut => outputs[idx] = Some(Err(fetch_timeout(&urls[idx]))),
            }
        }

        // 2. One batch call to the first fetch-capable provider.
        let generic_urls: Vec<String> = generic.iter().map(|(idx, _)| urls[*idx].clone()).collect();
        let mut request_ids = Vec::new();
        let mut usage = None;
        let mut first_try: Option<Vec<std::result::Result<FetchedPage, String>>> = None;
        let mut first_name: &'static str = "";
        let mut rest_chain: Vec<SourceEntry> = chain.clone();
        if let Some(pos) = chain.iter().position(|entry| entry.spec.supports_fetch) {
            if !generic_urls.is_empty() {
                let entry = &chain[pos];
                let display = entry.spec.display;
                // The provider bounds itself by the deadline (it keeps
                // first-call pages if its retry runs out of time); the outer
                // timeout is a backstop a little later.
                let batch = tokio::time::timeout_at(
                    deadline + std::time::Duration::from_secs(1),
                    entry.provider.fetch_batch(&generic_urls, options, deadline),
                )
                .await;
                match batch {
                    Ok(None) => {}
                    Ok(Some(Ok(found))) => {
                        request_ids = found.request_ids;
                        usage = found.usage;
                        first_try = Some(
                            found
                                .outcomes
                                .into_iter()
                                .map(|outcome| {
                                    outcome.map_err(|reason| format!("{display}: {reason}"))
                                })
                                .collect(),
                        );
                        first_name = entry.spec.name;
                        rest_chain = chain[pos + 1..].to_vec();
                    }
                    Ok(Some(Err(err))) => {
                        first_try =
                            Some(vec![Err(format!("{display}: {err}")); generic_urls.len()]);
                        first_name = entry.spec.name;
                        rest_chain = chain[pos + 1..].to_vec();
                    }
                    Err(_elapsed) => {
                        first_try = Some(vec![
                            Err(format!(
                                "{display}: batch extract timed out (LLM_SEARCH_TIMEOUT_SECONDS)"
                            ));
                            generic_urls.len()
                        ]);
                        first_name = entry.spec.name;
                        rest_chain = Vec::new();
                    }
                }
            }
        }

        // 3. Whatever the batch did not read continues down the chain.
        let sem = Arc::new(tokio::sync::Semaphore::new(
            self.config.enrich_concurrency.max(1),
        ));
        let mut set: tokio::task::JoinSet<(usize, Result<WebFetchOutput>)> =
            tokio::task::JoinSet::new();
        for (slot, (idx, reason)) in generic.into_iter().enumerate() {
            let url = urls[idx].clone();
            let prior = match first_try.as_ref().map(|outcomes| outcomes[slot].clone()) {
                Some(Ok(page)) => {
                    outputs[idx] = Some(Ok(fetch_output(
                        &url,
                        page,
                        limit,
                        crate::sources::SourceType::Generic,
                        reason,
                        Some(first_name.to_string()),
                    )));
                    continue;
                }
                Some(Err(prior)) => Some(prior),
                None => None,
            };
            if rest_chain.is_empty() && prior.is_some() {
                outputs[idx] = Some(Err(GrokSearchError::Provider(prior.unwrap_or_default())));
                continue;
            }
            let chain = rest_chain.clone();
            let options = options.clone();
            let permit = Arc::clone(&sem);
            set.spawn(async move {
                let _permit = permit.acquire_owned().await.ok();
                let fetched = tokio::time::timeout_at(
                    deadline,
                    generic_source_fetch_with(&chain, &url, &options),
                )
                .await;
                let outcome = match fetched {
                    Ok(Ok((page, served))) => Ok(fetch_output(
                        &url,
                        page,
                        limit,
                        crate::sources::SourceType::Generic,
                        reason,
                        Some(served.to_string()),
                    )),
                    Ok(Err(err)) => Err(match prior {
                        Some(prior) => GrokSearchError::Provider(format!("{prior}; {err}")),
                        None => err,
                    }),
                    Err(_elapsed) => Err(fetch_timeout(&url)),
                };
                (idx, outcome)
            });
        }
        while let Some(joined) = set.join_next().await {
            if let Ok((idx, outcome)) = joined {
                outputs[idx] = Some(outcome);
            }
        }
        let outcomes = outputs
            .into_iter()
            .enumerate()
            .map(|(idx, outcome)| outcome.unwrap_or_else(|| Err(fetch_timeout(&urls[idx]))))
            .collect();
        (outcomes, request_ids, usage)
    }

    /// Generic fetch through the source chain, bounded by the call's shared
    /// `deadline`. Mirrors how inline enrichment already wraps the same
    /// helper: without this, each chain position would get its own full
    /// client timeout and the chain would multiply the configured budget.
    /// Returns the page and the name of the provider that served it.
    async fn web_fetch_raw(
        &self,
        url: &str,
        deadline: tokio::time::Instant,
        options: &FetchOptions,
    ) -> Result<(FetchedPage, &'static str)> {
        let chain = self.active_sources();
        match tokio::time::timeout_at(deadline, generic_source_fetch_with(&chain, url, options))
            .await
        {
            Ok(result) => result,
            Err(_elapsed) => Err(GrokSearchError::Timeout(format!(
                "web_fetch timed out fetching {url} through the source chain (LLM_SEARCH_TIMEOUT_SECONDS)"
            ))),
        }
    }

    pub async fn web_map(&self, url: &str, max_results: usize) -> Result<Vec<Source>> {
        let mut params = serde_json::Map::new();
        params.insert("limit".into(), serde_json::json!(max_results));
        Ok(self.web_map_with(url, &params).await?.sources)
    }

    /// `web_crawl`: Tavily Crawl with native fields. On a local server the
    /// pages go to a Markdown file and only an index comes back; on the HTTP
    /// transport they come back inline (see [`WebCrawlOutput`]).
    pub async fn web_crawl(
        &self,
        url: &str,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::model::tool::WebCrawlOutput> {
        let provider = self.site_provider()?;
        let crawl = provider.crawl(url, params).await.ok_or_else(|| {
            GrokSearchError::Provider(
                "the configured site provider has no crawl endpoint".to_string(),
            )
        })??;
        let failed = crawl
            .failed
            .iter()
            .map(|(url, error)| FetchFailure {
                url: url.clone(),
                error: error.clone(),
            })
            .collect();
        let mut out = crate::model::tool::WebCrawlOutput {
            url: url.to_string(),
            base_url: crawl.base_url.clone(),
            pages_count: crawl.pages.len(),
            failed,
            request_id: crawl.request_id.clone(),
            usage: crawl.usage.clone(),
            ..Default::default()
        };
        if self.config.write_crawl_files {
            let dir = self
                .config
                .output_dir
                .clone()
                .unwrap_or_else(|| std::env::temp_dir().join("llm-search"))
                .join("crawl");
            let (path, lines) =
                write_crawl_file(dir, url.to_string(), params.clone(), crawl.clone())
                    .await
                    .map_err(|err| {
                        GrokSearchError::Provider(format!(
                            "web_crawl could not write its result file: {err}"
                        ))
                    })?;
            out.file_path = Some(path.display().to_string());
            out.pages = crawl
                .pages
                .iter()
                .zip(lines)
                .map(|((url, page), line)| crate::model::tool::CrawlPageIndex {
                    url: url.clone(),
                    chars: page.content.chars().count(),
                    line: Some(line),
                })
                .collect();
            return Ok(out);
        }
        out.pages = crawl
            .pages
            .iter()
            .map(|(url, page)| crate::model::tool::CrawlPageIndex {
                url: url.clone(),
                chars: page.content.chars().count(),
                line: None,
            })
            .collect();
        let sources: Vec<Source> = crawl
            .pages
            .into_iter()
            .map(|(url, page)| {
                let mut source = Source::new(url, "tavily");
                source.title = page.title;
                source.content = Some(page.content);
                source.favicon = page.favicon;
                source
            })
            .collect();
        let mut uuid_buf = [0u8; uuid::fmt::Simple::LENGTH];
        let session_id = Uuid::new_v4().simple().encode_lower(&mut uuid_buf)[..12].to_string();
        let sources = Arc::new(sources);
        self.cache
            .lock()
            .await
            .set(self.tenant_cache_key(&session_id), sources.clone());
        let mut inline = (*sources).clone();
        out.truncated =
            apply_response_budget(0, &mut inline, self.config.response_max_chars, &session_id);
        out.sources = inline;
        out.session_id = Some(session_id);
        Ok(out)
    }

    /// `web_search` with `mode=research`: start (or resume) a Tavily research
    /// task and wait for the report up to the wait limit; past it, return the
    /// task id and status so the caller can tell the user and resume.
    async fn research_search(&self, input: WebSearchInput) -> Result<WebSearchOutput> {
        if input.include_domains.len() > 20 || input.exclude_domains.len() > 20 {
            return Err(GrokSearchError::InvalidParams(
                "research mode takes at most 20 include_domains and 20 exclude_domains".to_string(),
            ));
        }
        if input.research_id.is_none() && input.query.trim().is_empty() {
            return Err(GrokSearchError::InvalidParams(
                "web_search.query is required to start research".to_string(),
            ));
        }
        let provider = self.site_provider()?;
        let model = input
            .research
            .get("model")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("auto");
        let timing = ResearchTiming {
            initial: std::time::Duration::from_secs(2),
            max: std::time::Duration::from_secs(10),
            wait: self.config.research_max_wait.unwrap_or(if model == "mini" {
                std::time::Duration::from_secs(300)
            } else {
                std::time::Duration::from_secs(900)
            }),
        };
        let outcome = run_research(provider.as_ref(), &input, &timing).await?;

        let mut warnings = Vec::new();
        let mut ignored: Vec<String> = input
            .hints
            .names()
            .into_iter()
            .map(str::to_string)
            .collect();
        ignored.extend(input.tavily.keys().cloned());
        if input.recency_days.is_some() || input.time_range.is_some() {
            ignored.push("time window".to_string());
        }
        if input.start_date.is_some() || input.end_date.is_some() {
            ignored.push("start_date/end_date".to_string());
        }
        if input.max_results.is_some() {
            ignored.push("max_results".to_string());
        }
        if !ignored.is_empty() {
            warnings.push(format!("ignored in research mode: {}", ignored.join(", ")));
        }

        let mut uuid_buf = [0u8; uuid::fmt::Simple::LENGTH];
        let session_id = Uuid::new_v4().simple().encode_lower(&mut uuid_buf)[..12].to_string();
        match outcome {
            ResearchOutcome::Completed {
                id,
                content,
                sources,
                elapsed,
            } => {
                let sources_arc = Arc::new(sources);
                self.cache
                    .lock()
                    .await
                    .set(self.tenant_cache_key(&session_id), sources_arc.clone());
                let mut out_sources = (*sources_arc).clone();
                let truncated = apply_response_budget(
                    content.chars().count(),
                    &mut out_sources,
                    self.config.response_max_chars,
                    &session_id,
                );
                let source_log = build_source_log(&out_sources, Some(&input.query));
                Ok(WebSearchOutput {
                    session_id,
                    content,
                    sources_count: sources_arc.len(),
                    sources: out_sources,
                    search_provider: "tavily".to_string(),
                    truncated,
                    engine: self.config.search_engine.as_str().to_string(),
                    mode: Some("research".to_string()),
                    source_log,
                    warnings,
                    request_id: Some(id.clone()),
                    research_id: Some(id),
                    status: Some("completed".to_string()),
                    elapsed_seconds: Some(elapsed),
                    ..WebSearchOutput::default()
                })
            }
            ResearchOutcome::Pending {
                id,
                status,
                elapsed,
            } => {
                let notice = format!(
                    "Tavily research {id} is still running (status: {status}, waited {elapsed}s). Tell the user it is still in progress, then call web_search again with mode=\"research\" and research_id=\"{id}\" to keep waiting."
                );
                Ok(WebSearchOutput {
                    session_id,
                    content: notice.clone(),
                    search_provider: "tavily".to_string(),
                    engine: self.config.search_engine.as_str().to_string(),
                    mode: Some("research".to_string()),
                    notice: Some(notice),
                    warnings,
                    request_id: Some(id.clone()),
                    research_id: Some(id),
                    status: Some(status),
                    elapsed_seconds: Some(elapsed),
                    ..WebSearchOutput::default()
                })
            }
        }
    }

    /// The provider that serves `web_map` / `web_crawl`: a map-capable slot
    /// (Tavily), instantiated directly when the chain excludes it.
    fn site_provider(&self) -> Result<Arc<dyn SourceProvider>> {
        self.source_slots
            .iter()
            .find_map(|slot| match slot {
                SourceSlot::Active(entry) if entry.spec.supports_map => {
                    Some(entry.provider.clone())
                }
                _ => None,
            })
            .or_else(|| instantiate_source(&TAVILY_SPEC, &self.config, &self.http_client))
            .ok_or(GrokSearchError::MissingConfig("TAVILY_API_KEY"))
    }

    /// `web_map` with native Tavily map fields.
    pub async fn web_map_with(
        &self,
        url: &str,
        params: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<crate::model::source::MapResult> {
        // Only providers with a real site-map endpoint qualify (Tavily today);
        // the search-shaped `map` impls on the other providers are not a
        // substitute for actual URL discovery. web_map is a dedicated
        // capability, not part of the supplemental-source chain — an operator
        // whose LLM_SEARCH_SOURCE_PROVIDERS excludes Tavily from the chain
        // keeps map as long as TAVILY_API_KEY is configured, so a map-capable
        // provider absent from the chain is instantiated directly.
        self.site_provider()?.map_with(url, params).await
    }

    /// Runtime diagnostics with live connectivity probes against each configured backend.
    /// Returns provider availability flags, masked config, and per-provider reachability.
    pub async fn doctor(&self) -> serde_json::Value {
        self.doctor_scoped(None)
            .await
            .expect("an unscoped doctor call cannot fail validation")
    }

    /// [`doctor`](Self::doctor) limited to one provider (`brave`, `tavily`,
    /// `exa`, `tinyfish`, `firecrawl`, `grok`) or `all`. Providers outside the
    /// scope are reported with status `not_checked` and make no request.
    pub async fn doctor_scoped(&self, scope: Option<&str>) -> Result<serde_json::Value> {
        use crate::config::Transport;
        let scope = scope.map(|s| s.trim().to_ascii_lowercase());
        let scope = match scope.as_deref() {
            None | Some("") | Some("all") => None,
            Some(name @ ("brave" | "tavily" | "exa" | "tinyfish" | "firecrawl" | "grok")) => {
                Some(name.to_string())
            }
            Some(other) => {
                return Err(GrokSearchError::InvalidParams(format!(
                    "doctor.provider must be one of all, brave, tavily, exa, tinyfish, firecrawl, grok; got \"{other}\""
                )))
            }
        };
        let in_scope = |name: &str| scope.as_deref().map_or(true, |only| only == name);
        let grok_probe = if in_scope("grok") {
            self.probe_grok().await
        } else {
            Probe::not_checked()
        };
        let mut chain_probes = std::collections::HashMap::new();
        for name in ["brave", "tavily", "exa", "tinyfish", "firecrawl"] {
            let probe = if in_scope(name) {
                self.probe_chain_source(name).await
            } else {
                Probe::not_checked()
            };
            chain_probes.insert(name, probe);
        }
        let brave_probe = chain_probes.remove("brave").expect("probed");
        let tavily_probe = chain_probes.remove("tavily").expect("probed");
        let exa_probe = chain_probes.remove("exa").expect("probed");
        let tinyfish_probe = chain_probes.remove("tinyfish").expect("probed");
        let firecrawl_probe = chain_probes.remove("firecrawl").expect("probed");
        let source_chain: Vec<&str> = self
            .source_slots
            .iter()
            .filter_map(|slot| match slot {
                SourceSlot::Active(entry) => Some(entry.spec.name),
                SourceSlot::Missing { .. } => None,
            })
            .collect();

        // Surface the protocol the service actually dispatches with
        // (GROK_SEARCH_PROTOCOL); both talk to GROK_SEARCH_URL.
        let ai_api_url = self.config.grok_api_url.as_str();
        let (provider_label, ai_x_search_enabled) = match self.config.transport {
            Transport::Responses => ("grok_responses", self.config.x_search_enabled),
            // x_search has no chat-completions equivalent; report it disabled
            // rather than echoing a config flag that has no effect.
            Transport::ChatCompletions => ("grok_chat_completions", false),
        };

        Ok(serde_json::json!({
            "engine": self.config.search_engine.as_str(),
            "provider": provider_label,
            "transport": provider_label,
            "grok": {
                "api_url": ai_api_url,
                "model": self.default_model,
                "auth_mode": match self.config.grok_auth_mode {
                    AuthMode::ApiKey => "api_key",
                    AuthMode::OAuth => "oauth",
                },
                "auth_file": self.config
                    .grok_auth_file
                    .clone()
                    .or_else(crate::config::auth_path)
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "unavailable".to_string()),
                "web_search_enabled": self.config.web_search_enabled,
                "x_search_enabled": ai_x_search_enabled,
                "configured": self.ai.is_some(),
                "reachable": grok_probe.ok,
                "status": grok_probe.status(),
                "detail": grok_probe.detail,
            },
            "brave": {
                "api_url": self.config.brave_api_url,
                "enabled": self.config.brave_enabled,
                "reachable": brave_probe.ok,
                "status": brave_probe.status(),
                "detail": brave_probe.detail,
            },
            "tavily": {
                "api_url": self.config.tavily_api_url,
                "enabled": self.config.tavily_enabled,
                "reachable": tavily_probe.ok,
                "status": tavily_probe.status(),
                "detail": tavily_probe.detail,
            },
            "exa": {
                "api_url": self.config.exa_api_url,
                "enabled": self.config.exa_enabled,
                "reachable": exa_probe.ok,
                "status": exa_probe.status(),
                "detail": exa_probe.detail,
            },
            "tinyfish": {
                "search_api_url": self.config.tinyfish_search_api_url,
                "fetch_api_url": self.config.tinyfish_fetch_api_url,
                "enabled": self.config.tinyfish_enabled,
                "reachable": tinyfish_probe.ok,
                "status": tinyfish_probe.status(),
                "detail": tinyfish_probe.detail,
            },
            "firecrawl": {
                "api_url": self.config.firecrawl_api_url,
                "enabled": self.config.firecrawl_enabled,
                "reachable": firecrawl_probe.ok,
                "status": firecrawl_probe.status(),
                "detail": firecrawl_probe.detail,
            },
            "source_chain": source_chain,
            // Whether the config file was read at all, and why not when it was
            // not. Without this a rejected file is indistinguishable from an
            // absent one: both leave every setting at its default, and the
            // only account of the difference went to stderr, which an MCP
            // client swallows.
            "config_file": {
                "path": self.config
                    .config_file_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                "state": self.config.config_file_state.as_str(),
                "detail": self.config.config_file_state.detail(),
            },
            "default_extra_sources": self.config.default_extra_sources,
            "fallback_sources": self.config.fallback_sources,
            "cache_size": self.config.cache_size,
            "timeout_seconds": self.config.timeout.as_secs(),
            "github_token": self.config.github_token_status(),
            "redacted": self.config.redacted_diagnostics(),
            "note": "doctor performs live provider requests and may consume quota; provider_status checks configuration only."
        }))
    }

    /// Configuration-only status: which providers are configured, the order
    /// each tool will try them in, and the active engine. Makes no provider
    /// request, so it costs no quota, and never returns a secret.
    pub fn provider_status(&self) -> serde_json::Value {
        use crate::config::Transport;
        let active = self.active_sources();
        let active_names: Vec<&'static str> = active.iter().map(|e| e.spec.name).collect();
        let has = |name: &str| active_names.contains(&name);
        let rest: Vec<&'static str> = active_names
            .iter()
            .copied()
            .filter(|name| !matches!(*name, "brave" | "tavily"))
            .collect();
        let parallel: Vec<&'static str> = ["brave", "tavily"]
            .into_iter()
            .filter(|name| has(name))
            .collect();
        let web_search = match self.config.search_engine {
            SearchEngine::Brave => serde_json::json!({
                "quick": router_mode_order(&active_names, RouterMode::Quick, &self.config),
                "deep": router_mode_order(&active_names, RouterMode::Deep, &self.config),
                "verify": { "parallel": parallel, "then": rest },
            }),
            SearchEngine::Grok => serde_json::json!({
                "answer": "grok",
                "enrichment_and_fallback": active_names,
            }),
        };
        let web_map = if instantiate_source(&TAVILY_SPEC, &self.config, &self.http_client).is_some()
        {
            "tavily"
        } else {
            "unavailable (set TAVILY_API_KEY)"
        };
        let provider = |spec: &ProviderSpec| {
            serde_json::json!({
                "configured": provider_key_set(spec, &self.config),
                "enabled": provider_enabled(spec, &self.config),
                "active": has(spec.name),
            })
        };
        serde_json::json!({
            "checked_at": crate::clock::now_rfc3339(),
            "engine": self.config.search_engine.as_str(),
            "providers": {
                "brave": provider(&BRAVE_SPEC),
                "tavily": provider(&TAVILY_SPEC),
                "exa": provider(&EXA_SPEC),
                "tinyfish": provider(&TINYFISH_SPEC),
                "firecrawl": provider(&FIRECRAWL_SPEC),
                "grok": {
                    "configured": self.ai.is_some(),
                    "transport": match self.config.transport {
                        Transport::Responses => "grok_responses",
                        Transport::ChatCompletions => "grok_chat_completions",
                    },
                    "required": self.config.search_engine == SearchEngine::Grok,
                    "detail": self.ai_unavailable,
                },
            },
            "web_search_order": web_search,
            "web_fetch_order": active
                .iter()
                .filter(|entry| entry.spec.supports_fetch)
                .map(|entry| entry.spec.name)
                .collect::<Vec<_>>(),
            "web_map": web_map,
            "web_crawl": web_map,
            "note": "provider_status makes no provider request; use doctor for a live check (may consume quota)."
        })
    }

    /// Probe one chain provider by name: live search for an active slot, a
    /// skip marker naming the missing key otherwise.
    async fn probe_chain_source(&self, name: &str) -> Probe {
        let active = self.source_slots.iter().find_map(|slot| match slot {
            SourceSlot::Active(entry) if entry.spec.name == name => Some(entry.clone()),
            _ => None,
        });
        match active {
            Some(entry) => probe_source(entry.provider.as_ref(), "https://example.com").await,
            None => {
                let key_var = CANONICAL_SOURCE_ORDER
                    .iter()
                    .find(|spec| spec.name == name)
                    .map(|spec| spec.key_var)
                    .unwrap_or("API key");
                Probe::skipped(format!("{key_var} not configured"))
            }
        }
    }

    async fn probe_grok(&self) -> Probe {
        // Mirror the real search shape so the probe doesn't fail the
        // adapter's "web_search tool intent" pre-check.
        let mut tools = Vec::new();
        if self.config.web_search_enabled {
            tools.push(SearchTool::web_search());
        }
        let request = SearchRequest {
            model: self.default_model.clone(),
            system: None,
            messages: vec![SearchMessage {
                role: "user".to_string(),
                content: vec![ContentBlock::text("ping")],
            }],
            tools,
        };
        let Some(ai) = self.ai.as_ref() else {
            return Probe::skipped(format!(
                "Grok not configured: {}",
                self.ai_unavailable
                    .as_deref()
                    .unwrap_or("no Grok configuration")
            ));
        };
        match ai.search(&request).await {
            Ok(_) => Probe::ok("grok responded"),
            Err(err) => Probe::failed(err.to_string()),
        }
    }

    fn build_search_request(
        &self,
        input: &WebSearchInput,
        extra_sources: &[Source],
    ) -> SearchRequest {
        let mut content = input.query.clone();
        if let Some(platform) = input.platform.as_deref().filter(|value| !value.is_empty()) {
            content.push_str("\n\nFocus platform: ");
            content.push_str(platform);
        }
        if let Some(days) = input.recency_days {
            content.push_str(&format!(
                "\n\nRestrict evidence to sources published within the last {days} day(s)."
            ));
        }
        if let Some(range) = input.time_range.as_deref() {
            content.push_str(&format!(
                "\n\nRestrict evidence to sources published within the last {range}."
            ));
        }
        match (input.start_date.as_deref(), input.end_date.as_deref()) {
            (Some(start), Some(end)) => content.push_str(&format!(
                "\n\nRestrict evidence to sources published from {start} to {end}."
            )),
            (Some(start), None) => content.push_str(&format!(
                "\n\nRestrict evidence to sources published on or after {start}."
            )),
            (None, Some(end)) => content.push_str(&format!(
                "\n\nRestrict evidence to sources published on or before {end}."
            )),
            (None, None) => {}
        }
        if !input.include_domains.is_empty() {
            content.push_str("\n\nPrefer sources from: ");
            content.push_str(&input.include_domains.join(", "));
        }
        if !input.exclude_domains.is_empty() {
            content.push_str("\n\nDo not cite sources from: ");
            content.push_str(&input.exclude_domains.join(", "));
        }
        if !extra_sources.is_empty() {
            content.push_str("\n\nAdditional sources:\n");
            for source in extra_sources {
                content.push_str("- ");
                content.push_str(&source.url);
                if let Some(title) = &source.title {
                    content.push_str(" | ");
                    content.push_str(title);
                }
                content.push('\n');
            }
        }

        SearchRequest {
            model: input
                .model
                .clone()
                .unwrap_or_else(|| self.default_model.clone()),
            system: Some("Answer concisely with factual claims grounded in web search sources. Prefer primary sources. If sources are weak or unavailable, say so.".to_string()),
            messages: vec![SearchMessage {
                role: "user".to_string(),
                content: vec![ContentBlock::text(content)],
            }],
            tools: vec![SearchTool::web_search()],
        }
    }
}

/// Outcome of the speculative source fan-out: the sources, the name of the
/// chain provider that served them (`None` when nothing answered), plus a
/// per-provider account of what happened. `notes` stays empty on the
/// first-provider happy path; earlier dead ends are recorded so a request
/// that ends with zero sources can say why.
struct RawSources {
    sources: Vec<Source>,
    origin: Option<&'static str>,
    notes: Vec<SourceNote>,
}

impl RawSources {
    fn found(sources: Vec<Source>, origin: Option<&'static str>) -> Self {
        Self {
            sources,
            origin,
            notes: Vec::new(),
        }
    }

    fn empty(notes: Vec<SourceNote>) -> Self {
        Self {
            sources: Vec::new(),
            origin: None,
            notes,
        }
    }
}

/// Upstream error text is quoted verbatim by the provider layer, and a gateway
/// or intermediary can answer with an arbitrarily large HTML page. Notes now
/// reach the caller inside a tool error, which no response budget applies to,
/// so each one is capped where it is recorded.
const MAX_SOURCE_NOTE_CHARS: usize = 300;

/// What kind of dead end a provider hit, which is what decides the useful
/// remedy. Collapsing these into "did it work" would hand every failure the
/// same advice, and the advice differs sharply: a deliberately disabled
/// fan-out is not a broken upstream, and neither is an honest empty result.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NoteKind {
    /// The provider answered normally and simply had no matches, or was
    /// deliberately skipped by the filter gate. The query is the thing to
    /// change.
    NoResults,
    /// The operator switched this path off — a disabled provider, or a source
    /// budget of zero. Nothing is broken; the configuration says no.
    Config,
    /// A missing credential or an upstream failure. Something has to be fixed
    /// before this path can work at all.
    Broken,
}

/// One provider's account of why it produced nothing.
struct SourceNote {
    kind: NoteKind,
    detail: String,
}

impl SourceNote {
    fn new(kind: NoteKind, detail: impl Into<String>) -> Self {
        Self {
            kind,
            detail: truncate_note(detail.into()),
        }
    }

    fn no_results(detail: impl Into<String>) -> Self {
        Self::new(NoteKind::NoResults, detail)
    }

    fn config(detail: impl Into<String>) -> Self {
        Self::new(NoteKind::Config, detail)
    }

    fn broken(detail: impl Into<String>) -> Self {
        Self::new(NoteKind::Broken, detail)
    }
}

fn truncate_note(mut detail: String) -> String {
    if detail.chars().count() <= MAX_SOURCE_NOTE_CHARS {
        return detail;
    }
    let cut = detail
        .char_indices()
        .nth(MAX_SOURCE_NOTE_CHARS)
        .map(|(idx, _)| idx)
        .unwrap_or(detail.len());
    detail.truncate(cut);
    detail.push('…');
    detail
}

/// One-line account of why no source provider produced anything, for the
/// zero-source failure message. Carries provider error text (status codes,
/// upstream detail) but never any credential: keys travel in headers, and the
/// provider errors these notes wrap quote only the endpoint and response body.
fn source_diagnosis(notes: &[SourceNote]) -> String {
    if notes.is_empty() {
        "no source provider was consulted".to_string()
    } else {
        notes
            .iter()
            .map(|note| note.detail.as_str())
            .collect::<Vec<_>>()
            .join("; ")
    }
}

/// Keep only what the rest of the pipeline can act on, or explain what the
/// provider did instead.
///
/// Both normalizers accept an entry whose `url` is an empty string, and
/// `merge_sources` discards those — but only far downstream, after the caller
/// has already truncated to its source budget. Left in place, a blank entry
/// consumes a budget slot and silently displaces real evidence: a budget of 1
/// against `[blank, valid]` used to keep the blank, drop the valid source in
/// `merge_sources`, and fail the whole request. Filtering here means truncation
/// only ever applies to sources that will survive.
fn usable_or_note(
    provider: &str,
    sources: Vec<Source>,
) -> std::result::Result<Vec<Source>, SourceNote> {
    let returned = sources.len();
    let usable: Vec<Source> = sources
        .into_iter()
        .filter(|source| !source.url.trim().is_empty())
        .collect();
    if !usable.is_empty() {
        return Ok(usable);
    }
    // An empty answer is an honest no-match; an answer made entirely of blanks
    // is a provider returning something unusable, which is a different problem
    // with a different remedy.
    Err(if returned == 0 {
        SourceNote::no_results(format!("{provider}: no results"))
    } else {
        SourceNote::broken(format!("{provider}: results carried no usable URLs"))
    })
}

/// What would actually change the outcome, composed from every kind of dead
/// end present rather than one chosen by precedence. Mixed outcomes are the
/// common case — Tavily answering normally with no matches while Firecrawl has
/// no key — and there a different query can still succeed through the healthy
/// provider without anyone touching the other one's credentials. Suppressing
/// that advice would rule out the move that works; equally, the "do not just
/// repeat this" signal has to survive whenever nothing answered normally,
/// since that is what a client model needs to stop retrying a dead path.
fn fallback_remediation(notes: &[SourceNote]) -> String {
    if notes.is_empty() {
        return "Retrying will not help until the Grok upstream or the source-provider credentials are fixed.".to_string();
    }
    let has = |kind: NoteKind| notes.iter().any(|note| note.kind == kind);
    let mut parts = Vec::new();
    if has(NoteKind::NoResults) {
        parts.push("A source provider answered normally with no matches, so a different query or looser filters may help.");
    }
    if has(NoteKind::Config) {
        // Only a provider disabled through its own switch reaches this far: the
        // other producer of `Config` is the zero fan-out, which cannot happen
        // without a zero fallback budget, and `finalize_fallback` reports that
        // before asking for a remediation. Raising the budget would not
        // instantiate a switched-off provider, so it is not offered here.
        parts.push(
            "A source provider is switched off; enable it (TAVILY_ENABLED / EXA_ENABLED / TINYFISH_ENABLED / FIRECRAWL_ENABLED).",
        );
    }
    if has(NoteKind::Broken) {
        parts.push("A source provider is unusable until its credentials or upstream are fixed.");
    }
    if !has(NoteKind::NoResults) {
        // Deliberately not an absolute claim. A Grok timeout or 5xx clears on
        // its own, and an unchanged retry does succeed once it does — but not
        // before, which is what a client model looping on the same dead path
        // needs to hear.
        parts.push("Repeating this query unchanged will fail the same way until the condition above clears.");
    }
    parts.join(" ")
}

/// Why a source provider is absent from the service: the operator switched it
/// off (a configuration choice), or no key ever reached the process (something
/// to fix). The key hint names both channels because the HTTP transport
/// ignores server-side env keys entirely — a self-hoster who set the key in
/// their compose file needs to be told that the request header is the only one
/// that counts there.
fn unavailable_note(
    provider: &str,
    enabled: bool,
    enable_var: &str,
    key_var: &str,
    header: &str,
) -> SourceNote {
    if enabled {
        SourceNote::broken(format!(
            "{provider}: no API key ({key_var} for stdio, {header} header for the HTTP transport)"
        ))
    } else {
        SourceNote::config(format!("{provider}: disabled via {enable_var}"))
    }
}

#[cfg(test)]
mod remediation_tests {
    use super::*;

    // A switched-off provider is the only `Config` note that can reach the
    // remediation — the zero fan-out is reported by `finalize_fallback` before
    // this runs — and raising the source budget cannot instantiate one, so that
    // advice must not appear.
    #[test]
    fn disabled_provider_is_told_to_enable_not_to_raise_the_budget() {
        let notes = vec![unavailable_note(
            "tavily",
            false,
            "TAVILY_ENABLED",
            "TAVILY_API_KEY",
            "x-tavily-api-key",
        )];
        let remediation = fallback_remediation(&notes);
        assert!(remediation.contains("TAVILY_ENABLED"), "{remediation}");
        assert!(
            !remediation.contains("GROK_SEARCH_FALLBACK_SOURCES"),
            "{remediation}"
        );
    }

    // A Grok timeout or 5xx clears on its own, so the no-retry line must stop
    // short of claiming a repeat can never work — while still telling a looping
    // client that repeating it *now* changes nothing.
    #[test]
    fn retry_guidance_is_conditional_not_absolute() {
        let notes = vec![SourceNote::broken("tavily: provider error: HTTP 500")];
        let remediation = fallback_remediation(&notes);
        assert!(
            remediation.contains("until the condition above clears"),
            "{remediation}"
        );
        assert!(!remediation.contains("will not help"), "{remediation}");
    }

    // A provider that answered normally leaves a reformulated query on the
    // table, so the no-retry line stays out of the way entirely.
    #[test]
    fn a_healthy_empty_result_keeps_the_retry_door_open() {
        let notes = vec![
            SourceNote::no_results("tavily: no results"),
            SourceNote::broken("firecrawl: no API key"),
        ];
        let remediation = fallback_remediation(&notes);
        assert!(
            remediation.contains("a different query or looser filters"),
            "{remediation}"
        );
        assert!(
            !remediation.contains("Repeating this query unchanged"),
            "{remediation}"
        );
    }
}

#[cfg(test)]
mod chain_tests {
    use super::*;

    fn http() -> reqwest::Client {
        crate::providers::http::build_client(std::time::Duration::from_secs(5))
    }

    fn slot_names(slots: &[SourceSlot]) -> Vec<String> {
        slots
            .iter()
            .map(|slot| match slot {
                SourceSlot::Active(entry) => format!("active:{}", entry.spec.name),
                SourceSlot::Missing { spec, .. } => format!("missing:{}", spec.name),
            })
            .collect()
    }

    #[test]
    fn default_chain_orders_all_configured_providers_canonically() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("TAVILY_API_KEY", "t"),
            ("EXA_API_KEY", "e"),
            ("TINYFISH_API_KEY", "f"),
            ("FIRECRAWL_API_KEY", "c"),
        ]);
        let slots = build_source_slots(&config, &http()).expect("slots");
        assert_eq!(
            slot_names(&slots),
            [
                "active:tavily",
                "active:exa",
                "active:tinyfish",
                "active:firecrawl"
            ]
        );
    }

    // Core providers keep their historical place in zero-source diagnostics
    // even when unconfigured; optional ones stay out unless named explicitly,
    // so "I never set up Exa" cannot read as a broken credential.
    #[test]
    fn default_chain_slots_missing_core_but_omits_missing_optional() {
        let config =
            Config::from_env_map([("LLM_SEARCH_PROVIDER", "grok"), ("TINYFISH_API_KEY", "f")]);
        let slots = build_source_slots(&config, &http()).expect("slots");
        assert_eq!(
            slot_names(&slots),
            ["missing:tavily", "active:tinyfish", "missing:firecrawl"]
        );
    }

    #[test]
    fn explicit_order_overrides_and_slots_every_named_provider() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("TINYFISH_API_KEY", "f"),
            ("LLM_SEARCH_SOURCE_PROVIDERS", "tinyfish, Exa"),
        ]);
        let slots = build_source_slots(&config, &http()).expect("slots");
        assert_eq!(slot_names(&slots), ["active:tinyfish", "missing:exa"]);
    }

    #[test]
    fn unknown_provider_name_fails_construction() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("LLM_SEARCH_SOURCE_PROVIDERS", "tavily,serpapi"),
        ]);
        let err = match build_source_slots(&config, &http()) {
            Err(err) => err,
            Ok(_) => panic!("must reject unknown provider name"),
        };
        assert!(err.to_string().contains("serpapi"), "{err}");
    }

    #[test]
    fn labels_follow_the_serving_provider() {
        assert_eq!(enrichment_label(Some("exa")), "exa_enrichment");
        assert_eq!(fallback_label(Some("tinyfish")), "tinyfish_fallback");
        assert_eq!(fallback_label(None), "tavily_fallback");
    }

    /// Search always answers with no results.
    struct EmptySearchProvider;
    #[async_trait]
    impl SourceProvider for EmptySearchProvider {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
        async fn fetch(&self, url: &str) -> Result<FetchedPage> {
            Ok(FetchedPage::text(format!("empty provider fetch {url}")))
        }
        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    /// Serves sources whose provider label is the provider's own name.
    struct NamedSourceProvider(&'static str);
    #[async_trait]
    impl SourceProvider for NamedSourceProvider {
        async fn search_sources(
            &self,
            _query: &str,
            max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok((0..max_results)
                .map(|idx| Source::new(format!("https://{}.example/{idx}", self.0), self.0))
                .collect())
        }
        async fn fetch(&self, _url: &str) -> Result<FetchedPage> {
            Ok(FetchedPage::text(format!("content from {}", self.0)))
        }
        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    struct ErrAiProvider;
    #[async_trait]
    impl AiProvider for ErrAiProvider {
        async fn search(&self, _request: &SearchRequest) -> Result<SearchResponse> {
            Err(GrokSearchError::Provider("ai down".to_string()))
        }
    }

    fn chain_service(slots: Vec<SourceSlot>, ai: Arc<dyn AiProvider>) -> SearchService {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "fake"),
        ]);
        SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(ai),
            ai_unavailable: None,
            source_slots: Arc::new(slots),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(5)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        }
    }

    fn concise_input() -> WebSearchInput {
        WebSearchInput {
            query: "chain test".to_string(),
            // Opt out of inline content so no real HTTP enrichment runs.
            include_content: Some(false),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn chain_falls_through_to_next_provider_and_labels_fallback_by_name() {
        let svc = chain_service(
            vec![
                SourceSlot::Active(SourceEntry {
                    spec: &TAVILY_SPEC,
                    provider: Arc::new(EmptySearchProvider),
                }),
                SourceSlot::Active(SourceEntry {
                    spec: &TINYFISH_SPEC,
                    provider: Arc::new(NamedSourceProvider("tinyfish")),
                }),
            ],
            Arc::new(ErrAiProvider),
        );
        let out = svc
            .web_search(concise_input())
            .await
            .expect("fallback output");
        assert!(out.fallback_used);
        assert!(!out.sources.is_empty());
        assert!(
            out.sources
                .iter()
                .all(|source| source.provider == "tinyfish_fallback"),
            "expected tinyfish_fallback labels, got: {:?}",
            out.sources
                .iter()
                .map(|source| source.provider.clone())
                .collect::<Vec<_>>()
        );
    }

    #[tokio::test]
    async fn filtered_requests_skip_filter_blind_providers_but_use_capable_ones() {
        // Firecrawl sits FIRST here yet must be skipped for a filtered
        // request; TinyFish (filter-capable) serves instead.
        let svc = chain_service(
            vec![
                SourceSlot::Active(SourceEntry {
                    spec: &FIRECRAWL_SPEC,
                    provider: Arc::new(NamedSourceProvider("firecrawl")),
                }),
                SourceSlot::Active(SourceEntry {
                    spec: &TINYFISH_SPEC,
                    provider: Arc::new(NamedSourceProvider("tinyfish")),
                }),
            ],
            Arc::new(ErrAiProvider),
        );
        let mut input = concise_input();
        input.include_domains = vec!["example.com".to_string()];
        let out = svc.web_search(input).await.expect("fallback output");
        assert!(
            out.sources
                .iter()
                .all(|source| source.provider == "tinyfish_fallback"),
            "filter-blind firecrawl must not serve a filtered request: {:?}",
            out.sources
                .iter()
                .map(|source| source.provider.clone())
                .collect::<Vec<_>>()
        );
    }

    /// Search hangs far past any test deadline; fetch/map are instant.
    struct HangingSearchProvider;
    #[async_trait]
    impl SourceProvider for HangingSearchProvider {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(Vec::new())
        }
        async fn fetch(&self, url: &str) -> Result<FetchedPage> {
            Ok(FetchedPage::text(format!("hanging provider fetch {url}")))
        }
        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    // D-02: a hanging chain provider must be cut at the request's global
    // deadline, not granted a fresh full client timeout per chain position.
    #[tokio::test]
    async fn chain_is_bounded_by_the_global_deadline() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "fake"),
            ("LLM_SEARCH_TIMEOUT_SECONDS", "1"),
        ]);
        let svc = SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(ErrAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(vec![
                SourceSlot::Active(SourceEntry {
                    spec: &TAVILY_SPEC,
                    provider: Arc::new(HangingSearchProvider),
                }),
                SourceSlot::Active(SourceEntry {
                    spec: &TINYFISH_SPEC,
                    provider: Arc::new(NamedSourceProvider("tinyfish")),
                }),
            ]),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(5)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        };
        let started = std::time::Instant::now();
        let err = svc
            .web_search(concise_input())
            .await
            .expect_err("deadline-cut chain with a failed AI yields zero sources");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "chain must be cut at the ~1s deadline, took {:?}",
            started.elapsed()
        );
        assert!(
            err.to_string().contains("request deadline reached"),
            "error must name the deadline: {err}"
        );
    }

    /// Generic fetch hangs; search is instant and empty.
    struct HangingFetchProvider;
    #[async_trait]
    impl SourceProvider for HangingFetchProvider {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
        async fn fetch(&self, _url: &str) -> Result<FetchedPage> {
            tokio::time::sleep(std::time::Duration::from_secs(60)).await;
            Ok(FetchedPage::text("never reached"))
        }
        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    // D-02 on the direct web_fetch path: two hanging chain providers must
    // share one request budget, not take a full client timeout each.
    #[tokio::test]
    async fn web_fetch_chain_is_bounded_by_one_deadline() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "fake"),
            ("LLM_SEARCH_TIMEOUT_SECONDS", "1"),
        ]);
        let svc = SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(vec![
                SourceSlot::Active(SourceEntry {
                    spec: &TAVILY_SPEC,
                    provider: Arc::new(HangingFetchProvider),
                }),
                SourceSlot::Active(SourceEntry {
                    spec: &FIRECRAWL_SPEC,
                    provider: Arc::new(HangingFetchProvider),
                }),
            ]),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(5)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        };
        let started = std::time::Instant::now();
        let err = svc
            .web_fetch("https://example.com/page", None)
            .await
            .expect_err("hanging chain must time out");
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "web_fetch must be cut at the ~1s deadline, took {:?}",
            started.elapsed()
        );
        assert!(
            matches!(err, GrokSearchError::Timeout(_)),
            "expected a Timeout error, got: {err:?}"
        );
    }

    // web_map is a dedicated Tavily capability: excluding Tavily from the
    // supplemental chain must not break map while TAVILY_API_KEY is set.
    #[tokio::test]
    async fn web_map_survives_a_chain_that_excludes_tavily() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "fake"),
            ("TAVILY_API_KEY", "fake-tavily"),
            ("LLM_SEARCH_SOURCE_PROVIDERS", "tinyfish"),
        ]);
        let svc = SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(vec![SourceSlot::Active(SourceEntry {
                spec: &TINYFISH_SPEC,
                provider: Arc::new(NamedSourceProvider("tinyfish")),
            })]),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(5)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        };
        // The chain has no map-capable slot, so web_map instantiates Tavily
        // from config; the fake key means the call itself fails upstream, but
        // it must NOT fail as MissingConfig("TAVILY_API_KEY").
        let err = svc
            .web_map("https://example.com", 3)
            .await
            .expect_err("fake key cannot reach real Tavily");
        assert!(
            !matches!(err, GrokSearchError::MissingConfig(_)),
            "web_map must not report missing config when TAVILY_API_KEY is set: {err:?}"
        );
    }

    #[tokio::test]
    async fn enrichment_sources_carry_the_serving_providers_label() {
        // Grok succeeds (FakeAiProvider is verifiable); the supplemental
        // sources come from the second chain slot after the first is empty,
        // and must be labeled {provider}_enrichment.
        let svc = chain_service(
            vec![
                SourceSlot::Active(SourceEntry {
                    spec: &TAVILY_SPEC,
                    provider: Arc::new(EmptySearchProvider),
                }),
                SourceSlot::Active(SourceEntry {
                    spec: &EXA_SPEC,
                    provider: Arc::new(NamedSourceProvider("exa")),
                }),
            ],
            Arc::new(FakeAiProvider),
        );
        let out = svc
            .web_search(concise_input())
            .await
            .expect("search output");
        assert!(!out.fallback_used);
        assert!(
            out.sources
                .iter()
                .any(|source| source.provider == "exa_enrichment"),
            "expected exa_enrichment labels, got: {:?}",
            out.sources
                .iter()
                .map(|source| source.provider.clone())
                .collect::<Vec<_>>()
        );
    }
}

/// Router-mode results when the caller does not ask for a count.
const ROUTER_DEFAULT_RESULTS: usize = 5;
/// Router-mode result ceiling, as in the DeepSeek plugin.
const ROUTER_MAX_RESULTS: usize = 20;
/// Per-result cap on Tavily inline page text in deep / verify.
const ROUTER_RAW_CONTENT_CHARS: usize = 6_000;
/// Per-provider cap on a recorded fallback error.
const FALLBACK_ERROR_CHARS: usize = 500;

/// Router mode for `web_search` (`LLM_SEARCH_PROVIDER=brave`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RouterMode {
    Quick,
    Deep,
    Verify,
}

impl RouterMode {
    fn parse(value: Option<&str>) -> Result<Self> {
        match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
            None | Some("") | Some("quick") => Ok(RouterMode::Quick),
            Some("deep") => Ok(RouterMode::Deep),
            Some("verify") => Ok(RouterMode::Verify),
            Some(other) => Err(GrokSearchError::InvalidParams(format!(
                "mode must be \"quick\", \"deep\", \"verify\", or \"research\", got \"{other}\""
            ))),
        }
    }

    fn as_str(&self) -> &'static str {
        match self {
            RouterMode::Quick => "quick",
            RouterMode::Deep => "deep",
            RouterMode::Verify => "verify",
        }
    }
}

/// Search options for one provider in one router mode: Tavily goes advanced
/// with an answer and inline page text in deep / verify; every other call is
/// a plain search.
fn router_options(provider: &str, mode: RouterMode) -> SearchOptions {
    if provider == "tavily" && mode != RouterMode::Quick {
        SearchOptions {
            depth: crate::model::search::SearchDepth::Advanced,
            include_answer: true,
            include_raw_content: true,
            raw_content_max_chars: ROUTER_RAW_CONTENT_CHARS,
            ..SearchOptions::default()
        }
    } else {
        SearchOptions::default()
    }
}

/// [`router_options`] plus the caller's hints, and the Tavily-only fields
/// for Tavily. Every provider gets the hints; only hint-capable ones use them.
fn router_options_for(
    provider: &str,
    mode: RouterMode,
    hints: &crate::model::search::SearchHints,
    tavily: &serde_json::Map<String, serde_json::Value>,
) -> SearchOptions {
    let mut options = router_options(provider, mode);
    options.hints = hints.clone();
    if provider == "tavily" {
        options.tavily = tavily.clone();
    }
    options
}

/// Everything one router-mode search needs, shared by every attempt.
struct RouterCall<'a> {
    query: &'a str,
    max_results: usize,
    filters: &'a SearchFilters,
    mode: RouterMode,
    deadline: tokio::time::Instant,
    hints: &'a crate::model::search::SearchHints,
    tavily: &'a serde_json::Map<String, serde_json::Value>,
}

/// One router attempt against one provider. `Err` carries the reason it did
/// not serve: skipped for filters, timed out, upstream error, or no results.
async fn router_attempt(
    entry: &SourceEntry,
    call: &RouterCall<'_>,
) -> std::result::Result<DetailedSearch, String> {
    let name = entry.spec.name;
    if !filters_honored(entry.spec, call.filters) {
        return Err(format!(
            "skipped (request carries domain/recency filters, which {name} cannot honor)"
        ));
    }
    let options = router_options_for(name, call.mode, call.hints, call.tavily);
    let attempt = tokio::time::timeout_at(
        call.deadline,
        entry
            .provider
            .search_detailed(call.query, call.max_results, call.filters, &options),
    )
    .await;
    match attempt {
        Err(_elapsed) => Err("timed out (request deadline reached)".to_string()),
        Ok(Err(err)) => Err(crate::model::source::truncate_with_marker(
            &err.to_string(),
            FALLBACK_ERROR_CHARS,
        )),
        Ok(Ok(mut found)) => {
            found.sources.retain(|source| !source.url.trim().is_empty());
            found.sources.truncate(call.max_results);
            if found.sources.is_empty() && found.answer.is_none() {
                Err("no results".to_string())
            } else {
                Ok(found)
            }
        }
    }
}

/// [`router_attempt`] for an optional provider, tagged with its name; `None`
/// when the provider is not configured.
async fn router_attempt_opt(
    entry: Option<&SourceEntry>,
    call: &RouterCall<'_>,
) -> Option<(&'static str, std::result::Result<DetailedSearch, String>)> {
    let entry = entry?;
    Some((entry.spec.name, router_attempt(entry, call).await))
}

/// Try `order` in sequence; the first provider with a usable result serves.
/// Every provider tried before it lands in `fallbacks`.
async fn router_first_hit(
    order: &[SourceEntry],
    call: &RouterCall<'_>,
    fallbacks: &mut Vec<SearchFallback>,
) -> Option<(String, DetailedSearch)> {
    for entry in order {
        match router_attempt(entry, call).await {
            Ok(found) => return Some((entry.spec.name.to_string(), found)),
            Err(error) => fallbacks.push(SearchFallback {
                provider: entry.spec.name.to_string(),
                error,
            }),
        }
    }
    None
}

/// Warnings for parameters the serving provider(s) could not use. `served`
/// lists the providers whose results are in the response.
fn ignored_param_warnings(
    served: &[&str],
    hints: &crate::model::search::SearchHints,
    tavily: &serde_json::Map<String, serde_json::Value>,
    tavily_options: &SearchOptions,
) -> Vec<String> {
    let mut warnings = Vec::new();
    let tavily_served = served.contains(&"tavily");
    if !tavily.is_empty() && !tavily_served {
        let names: Vec<&str> = tavily.keys().map(String::as_str).collect();
        warnings.push(format!(
            "Tavily-only parameters ignored because {} served the result: {}",
            served.join(", "),
            names.join(", ")
        ));
    }
    if tavily_served {
        warnings.extend(crate::providers::tavily::tavily_dropped_hints(
            tavily_options,
        ));
    }
    let hint_names = hints.names();
    if !hint_names.is_empty() {
        for name in served {
            let capable = CANONICAL_SOURCE_ORDER
                .iter()
                .find(|spec| spec.name == *name)
                .is_some_and(|spec| spec.honors_hints);
            if !capable {
                warnings.push(format!(
                    "{name} cannot use {}; ignored for its results",
                    hint_names.join(", ")
                ));
            }
        }
    }
    warnings
}

/// The `content` of a router-mode result: the provider answer when there is
/// one, otherwise a pointer to the structured sources.
fn render_router_content(
    provider: &str,
    mode: RouterMode,
    count: usize,
    answer: Option<&str>,
) -> String {
    match answer {
        Some(answer) => format!(
            "{answer}\n\n(Answer generated by Tavily from {count} result(s), mode={}; verify it against `sources` and cite them by `source_log` id.)",
            mode.as_str()
        ),
        None => format!(
            "{count} result(s) from {provider} (mode={}). This mode returns no AI-written answer: read `sources` (title, url, description, and inline content when present) and cite them by `source_log` id.",
            mode.as_str()
        ),
    }
}

/// `Search provider fallback used: …` disclosure, or `None` when the first
/// provider served.
fn fallback_notice(fallbacks: &[SearchFallback]) -> Option<String> {
    if fallbacks.is_empty() {
        return None;
    }
    let detail = fallbacks
        .iter()
        .map(|entry| format!("{}: {}", entry.provider, entry.error))
        .collect::<Vec<_>>()
        .join("; ");
    Some(format!(
        "Search provider fallback used: {detail}. Disclose this if the answer relies on fallback results."
    ))
}

/// Stable `fallback_reason` identifier for the first provider that did not
/// serve: `{provider}_no_results`, `{provider}_skipped`, `{provider}_timeout`,
/// or `{provider}_error`.
fn fallback_reason_id(entry: &SearchFallback) -> String {
    let kind = if entry.error == "no results" {
        "no_results"
    } else if entry.error.starts_with("skipped") {
        "skipped"
    } else if entry.error.starts_with("timed out") {
        "timeout"
    } else {
        "error"
    };
    format!("{}_{kind}", entry.provider)
}

/// Host of an HTTP(S) URL, if it parses.
fn domain_of(url: &str) -> Option<String> {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_string))
}

/// `source_log` for returned sources: `src_001`… in returned order, ranked,
/// stamped with one access time.
fn build_source_log(sources: &[Source], query: Option<&str>) -> Vec<SourceLogEntry> {
    let accessed_at = crate::clock::now_rfc3339();
    sources
        .iter()
        .enumerate()
        .map(|(idx, source)| SourceLogEntry {
            source_id: format!("src_{:03}", idx + 1),
            url: source.url.clone(),
            provider: Some(source.provider.to_string()),
            domain: domain_of(&source.url),
            title: source.title.clone(),
            rank: Some(idx + 1),
            query: query.map(str::to_string),
            accessed_at: Some(accessed_at.clone()),
            extracted_at: None,
            content_hash: None,
            status: "success".to_string(),
        })
        .collect()
}

/// SHA-256 of `text`, lowercase hex.
fn sha256_hex(text: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, text.as_bytes())
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Default model for every outgoing `SearchRequest`: `GROK_SEARCH_MODEL`,
/// whichever protocol `GROK_SEARCH_PROTOCOL` selects.
fn resolve_default_model(config: &Config) -> String {
    config.grok_model.clone()
}

/// Per-source label for the enrichment path: the serving chain provider's
/// name plus an `_enrichment` suffix ("tavily_enrichment", "exa_enrichment",
/// …). `None` (nothing served) keeps the historical "tavily" default, though
/// it only ever labels an empty list.
fn enrichment_label(origin: Option<&'static str>) -> String {
    format!("{}_enrichment", origin.unwrap_or("tavily"))
}

/// Per-source label for the degraded path, `{provider}_fallback` (#30).
fn fallback_label(origin: Option<&'static str>) -> String {
    format!("{}_fallback", origin.unwrap_or("tavily"))
}

/// Maps a failed Grok call to a stable `fallback_reason` identifier. Kept at
/// enum-variant granularity on purpose: distinguishing timeout / auth / parse
/// from a generic provider failure is the diagnostically useful axis, while
/// sub-parsing HTTP status codes out of `Provider(String)` would be fragile.
/// `Provider` (and any other variant) preserves the legacy `grok_provider_error`.
fn grok_error_reason(err: &GrokSearchError) -> &'static str {
    match err {
        GrokSearchError::Timeout(_) => "grok_timeout",
        GrokSearchError::OAuth(_) => "grok_auth_error",
        GrokSearchError::Parse(_) => "grok_parse_error",
        _ => "grok_provider_error",
    }
}

fn grok_unverifiable_reason(response: &SearchResponse) -> Option<&'static str> {
    if response.content.trim().is_empty() {
        return Some("grok_content_empty");
    }
    if response.sources.is_empty() {
        return Some("grok_sources_empty");
    }
    None
}

fn apply_fetch_limit(
    url: &str,
    mut content: String,
    max_chars: Option<usize>,
    source_type: crate::sources::SourceType,
    fallback_reason: Option<String>,
) -> WebFetchOutput {
    let Some(limit) = max_chars else {
        let original_length = content.chars().count();
        return WebFetchOutput {
            url: url.to_string(),
            content,
            original_length,
            truncated: false,
            source_type,
            fallback_reason,
            provider: None,
            domain: None,
            content_hash: String::new(),
            extracted_at: String::new(),
            source_log: Vec::new(),
            ..WebFetchOutput::default()
        };
    };

    let mut count = 0usize;
    let mut cutoff: Option<usize> = None;
    for (byte_idx, _) in content.char_indices() {
        if count == limit {
            cutoff = Some(byte_idx);
            break;
        }
        count += 1;
    }

    match cutoff {
        Some(byte_idx) => {
            let extra = content[byte_idx..].chars().count();
            content.truncate(byte_idx);
            WebFetchOutput {
                url: url.to_string(),
                content,
                original_length: limit + extra,
                truncated: true,
                source_type,
                fallback_reason,
                provider: None,
                domain: None,
                content_hash: String::new(),
                extracted_at: String::new(),
                source_log: Vec::new(),
                ..WebFetchOutput::default()
            }
        }
        None => WebFetchOutput {
            url: url.to_string(),
            content,
            original_length: count,
            truncated: false,
            source_type,
            fallback_reason,
            provider: None,
            domain: None,
            content_hash: String::new(),
            extracted_at: String::new(),
            source_log: Vec::new(),
            ..WebFetchOutput::default()
        },
    }
}

/// Generic (non-specialist) content fetch via the configured source-provider
/// chain, first non-empty page wins. Shared by `web_fetch` and inline
/// enrichment so both agree on how an ordinary URL is retrieved once no
/// specialist extractor matches. Returns `MissingConfig` only when no
/// provider is configured at all; otherwise the last attempt's real error
/// surfaces, so users are not sent to debug config that is actually set.
async fn generic_source_fetch(chain: &[SourceEntry], url: &str) -> Result<FetchedPage> {
    generic_source_fetch_with(chain, url, &FetchOptions::default())
        .await
        .map(|(page, _served)| page)
}

/// [`generic_source_fetch`] with extraction options, also naming the
/// provider that served the page. Search-only providers (Brave) are skipped.
async fn generic_source_fetch_with(
    chain: &[SourceEntry],
    url: &str,
    options: &FetchOptions,
) -> Result<(FetchedPage, &'static str)> {
    let mut last_error: Option<GrokSearchError> = None;
    for entry in chain.iter().filter(|entry| entry.spec.supports_fetch) {
        match entry.provider.fetch_with(url, options).await {
            Ok(page) if !page.content.trim().is_empty() => return Ok((page, entry.spec.name)),
            Ok(_) => {
                last_error = Some(GrokSearchError::Provider(format!(
                    "{} returned empty content for {url}",
                    entry.spec.display
                )))
            }
            Err(err) => last_error = Some(err),
        }
    }
    Err(last_error.unwrap_or(GrokSearchError::MissingConfig(SOURCE_PROVIDER_KEYS)))
}

/// Write a crawl result as one Markdown file under `dir` (created if needed,
/// owner-only permissions on Unix): a header, an index of pages with their
/// starting lines, then one `## [n] <url>` section per page. Returns the path
/// and each page's 1-based starting line, in page order.
async fn write_crawl_file(
    dir: std::path::PathBuf,
    url: String,
    params: serde_json::Map<String, serde_json::Value>,
    crawl: crate::model::source::CrawlResult,
) -> std::io::Result<(std::path::PathBuf, Vec<usize>)> {
    tokio::task::spawn_blocking(move || {
        let (text, lines) =
            render_crawl_markdown(&url, &params, &crawl, &crate::clock::now_rfc3339());
        std::fs::create_dir_all(&dir)?;
        let stamp: String = crate::clock::now_rfc3339()
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .collect();
        let host: String = url::Url::parse(&url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string))
            .unwrap_or_else(|| url.split('/').next().unwrap_or("site").to_string())
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '-' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        let mut uuid_buf = [0u8; uuid::fmt::Simple::LENGTH];
        let tag = Uuid::new_v4().simple().encode_lower(&mut uuid_buf)[..6].to_string();
        let path = dir.join(format!("{stamp}-{host}-{tag}.md"));
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        use std::io::Write;
        options.open(&path)?.write_all(text.as_bytes())?;
        Ok((path, lines))
    })
    .await
    .map_err(std::io::Error::other)?
}

/// Markdown for a crawl result plus each page section's 1-based start line.
fn render_crawl_markdown(
    url: &str,
    params: &serde_json::Map<String, serde_json::Value>,
    crawl: &crate::model::source::CrawlResult,
    crawled_at: &str,
) -> (String, Vec<usize>) {
    let sections: Vec<String> = crawl
        .pages
        .iter()
        .enumerate()
        .map(|(idx, (page_url, page))| {
            format!(
                "## [{}] {page_url}\n\n{}\n\n",
                idx + 1,
                page.content.trim_end()
            )
        })
        .collect();
    let mut header = format!(
        "# Crawl: {}\n\n- Start URL: {url}\n- Crawled at: {crawled_at}\n- Pages: {}\n",
        crawl.base_url.as_deref().unwrap_or(url),
        crawl.pages.len()
    );
    if let Some(id) = &crawl.request_id {
        header.push_str(&format!("- Tavily request id: {id}\n"));
    }
    if !params.is_empty() {
        header.push_str(&format!(
            "- Parameters: `{}`\n",
            serde_json::Value::Object(params.clone())
        ));
    }
    for (failed_url, reason) in &crawl.failed {
        header.push_str(&format!("- Not read: {failed_url} ({reason})\n"));
    }
    header.push_str("\n| # | URL | Chars | Line |\n|---|---|---|---|\n");
    let index_rows = crawl.pages.len();
    // Line numbers depend on the header length, which includes the rows that
    // carry them; the rows' count is fixed, so compute starts first.
    let header_lines = header.matches('\n').count() + index_rows + 1;
    let mut starts = Vec::with_capacity(sections.len());
    let mut line = header_lines + 1;
    for section in &sections {
        starts.push(line);
        line += section.matches('\n').count();
    }
    for (idx, ((page_url, page), start)) in crawl.pages.iter().zip(&starts).enumerate() {
        header.push_str(&format!(
            "| {} | {page_url} | {} | {start} |\n",
            idx + 1,
            page.content.chars().count()
        ));
    }
    header.push('\n');
    let mut text = header;
    for section in sections {
        text.push_str(&section);
    }
    (text, starts)
}

/// Research polling schedule: start at `initial`, grow ×1.5 up to `max`,
/// stop waiting after `wait` (the official tavily-mcp values are 2 s, 10 s,
/// and 5 / 15 minutes).
struct ResearchTiming {
    initial: std::time::Duration,
    max: std::time::Duration,
    wait: std::time::Duration,
}

enum ResearchOutcome {
    Completed {
        id: String,
        content: String,
        sources: Vec<Source>,
        elapsed: u64,
    },
    Pending {
        id: String,
        status: String,
        elapsed: u64,
    },
}

/// Body for `POST /research`.
fn research_body(input: &WebSearchInput) -> serde_json::Value {
    let mut body = serde_json::json!({ "input": input.query.trim() });
    let object = body.as_object_mut().expect("literal object");
    for (key, value) in &input.research {
        object.insert(key.clone(), value.clone());
    }
    if !input.include_domains.is_empty() {
        object.insert(
            "include_domains".into(),
            serde_json::json!(input.include_domains),
        );
    }
    if !input.exclude_domains.is_empty() {
        object.insert(
            "exclude_domains".into(),
            serde_json::json!(input.exclude_domains),
        );
    }
    body
}

/// Start (or resume) a research task and poll it per `timing`.
async fn run_research(
    provider: &dyn SourceProvider,
    input: &WebSearchInput,
    timing: &ResearchTiming,
) -> Result<ResearchOutcome> {
    let no_endpoint = || {
        GrokSearchError::Provider("the configured provider has no research endpoint".to_string())
    };
    let started = tokio::time::Instant::now();
    let mut interval = timing.initial;
    let id = match input.research_id.as_deref().map(str::trim) {
        Some(id) => {
            if id.is_empty()
                || !id
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
            {
                return Err(GrokSearchError::InvalidParams(format!(
                    "research_id \"{id}\" is not a Tavily request id"
                )));
            }
            id.to_string()
        }
        None => {
            let raw = provider
                .research_start(&research_body(input))
                .await
                .ok_or_else(no_endpoint)??;
            let id = raw
                .get("request_id")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    GrokSearchError::Provider("Tavily research returned no request_id".to_string())
                })?
                .to_string();
            // A fresh task is never done at once; wait before the first poll.
            tokio::time::sleep(interval.min(timing.wait)).await;
            id
        }
    };
    loop {
        // Once the task exists, a failed poll must not lose its id: the
        // task keeps running (and is paid for) on Tavily's side.
        let raw = match provider.research_get(&id).await.ok_or_else(no_endpoint)? {
            Ok(raw) => raw,
            Err(err @ GrokSearchError::InvalidParams(_)) => return Err(err),
            Err(err) => {
                return Err(GrokSearchError::Provider(format!(
                    "{err}; research {id} may still be running: call web_search with mode=\"research\" and research_id=\"{id}\" to resume"
                )))
            }
        };
        let status = raw
            .get("status")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("pending")
            .to_string();
        let elapsed = started.elapsed();
        match status.as_str() {
            "completed" => {
                let content = match raw.get("content") {
                    Some(serde_json::Value::String(text)) => text.clone(),
                    Some(other) if !other.is_null() => {
                        serde_json::to_string_pretty(other).unwrap_or_default()
                    }
                    _ => String::new(),
                };
                let sources = raw
                    .get("sources")
                    .and_then(serde_json::Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter_map(|item| {
                        let url = item.get("url").and_then(serde_json::Value::as_str)?;
                        let mut source = Source::new(url, "tavily");
                        if let Some(title) = item.get("title").and_then(serde_json::Value::as_str) {
                            source = source.with_title(title);
                        }
                        source.favicon = item
                            .get("favicon")
                            .and_then(serde_json::Value::as_str)
                            .map(str::to_string);
                        Some(source)
                    })
                    .collect();
                return Ok(ResearchOutcome::Completed {
                    id,
                    content,
                    sources,
                    elapsed: elapsed.as_secs(),
                });
            }
            "failed" => {
                let reason = raw
                    .get("error")
                    .or_else(|| raw.get("detail"))
                    .map(|value| match value {
                        serde_json::Value::String(text) => text.clone(),
                        other => other.to_string(),
                    })
                    .unwrap_or_else(|| "no reason given".to_string());
                return Err(GrokSearchError::Provider(format!(
                    "Tavily research {id} failed: {reason}"
                )));
            }
            _ => {}
        }
        if elapsed + interval > timing.wait {
            return Ok(ResearchOutcome::Pending {
                id,
                status,
                elapsed: elapsed.as_secs(),
            });
        }
        tokio::time::sleep(interval).await;
        interval = interval.mul_f64(1.5).min(timing.max);
    }
}

/// Outcome of the specialist stage for one URL.
enum SpecialistStage {
    /// A specialist rendered the page.
    Served(String, crate::sources::SourceType),
    /// Go generic; `Some(reason)` when a specialist matched and failed (D-01).
    Generic(Option<String>),
    /// The call's deadline ran out.
    TimedOut,
}

fn fetch_timeout(url: &str) -> GrokSearchError {
    GrokSearchError::Timeout(format!(
        "web_fetch timed out extracting {url} (LLM_SEARCH_TIMEOUT_SECONDS)"
    ))
}

/// Build the `web_fetch` output for a fetched page: cap, provenance, and the
/// provider's optional metadata.
fn fetch_output(
    url: &str,
    page: FetchedPage,
    limit: Option<usize>,
    source_type: crate::sources::SourceType,
    fallback_reason: Option<String>,
    provider: Option<String>,
) -> WebFetchOutput {
    let extras = crate::model::tool::FetchExtras {
        images: page.images,
        favicon: page.favicon,
        request_id: page.request_id,
        usage: page.usage,
    };
    let out = apply_fetch_limit(url, page.content, limit, source_type, fallback_reason);
    let mut out = finish_fetch_output(out, provider, crate::clock::now_rfc3339());
    out.extras = extras;
    out
}

/// Drain a per-URL JoinSet into input order; a task that panicked becomes a
/// provider error for its URL.
async fn collect_in_order(
    mut set: tokio::task::JoinSet<(usize, Result<WebFetchOutput>)>,
    urls: &[String],
) -> Vec<Result<WebFetchOutput>> {
    let mut slots: Vec<Option<Result<WebFetchOutput>>> = (0..urls.len()).map(|_| None).collect();
    while let Some(joined) = set.join_next().await {
        if let Ok((idx, outcome)) = joined {
            slots[idx] = Some(outcome);
        }
    }
    slots
        .into_iter()
        .enumerate()
        .map(|(idx, slot)| {
            slot.unwrap_or_else(|| {
                Err(GrokSearchError::Provider(format!(
                    "web_fetch task for {} did not complete",
                    urls[idx]
                )))
            })
        })
        .collect()
}

/// Fill `web_fetch` provenance: provider, domain, SHA-256 of the returned
/// content, extraction time, and a one-entry `source_log`.
fn finish_fetch_output(
    mut out: WebFetchOutput,
    provider: Option<String>,
    extracted_at: String,
) -> WebFetchOutput {
    let content_hash = sha256_hex(&out.content);
    let domain = domain_of(&out.url);
    out.source_log = vec![SourceLogEntry {
        source_id: "src_001".to_string(),
        url: out.url.clone(),
        provider: provider.clone(),
        domain: domain.clone(),
        extracted_at: Some(extracted_at.clone()),
        content_hash: Some(content_hash.clone()),
        status: "success".to_string(),
        ..SourceLogEntry::default()
    }];
    out.provider = provider;
    out.domain = domain;
    out.content_hash = content_hash;
    out.extracted_at = extracted_at;
    out
}

/// The serialized name of a specialist `SourceType` (`github_issue`, …).
fn source_type_label(kind: &crate::sources::SourceType) -> Option<String> {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
}

/// The keys that make a generic fetch possible at all. Named from one place so
/// the `web_fetch` error and the inline-enrichment note cannot drift apart.
const SOURCE_PROVIDER_KEYS: &str =
    "TAVILY_API_KEY, EXA_API_KEY, TINYFISH_API_KEY or FIRECRAWL_API_KEY";

/// Why an ordinary URL could not be enriched.
///
/// With an empty chain there is no source provider to fetch it, and that has
/// nothing to do with specialist extractors: those need no key and simply did
/// not match this URL. Reporting the specialist outcome here sends the reader
/// off to debug the wrong subsystem — it is how the reporter of issue #39 came
/// to believe specialists require API keys.
fn enrichment_failure_reason(chain: &[SourceEntry], specialist_reason: &str) -> String {
    if chain.is_empty() {
        format!("no source provider configured (set {SOURCE_PROVIDER_KEYS})")
    } else {
        specialist_reason.to_string()
    }
}

/// One enrichment outcome: the content to store plus any metadata backfill
/// harvested from the fetched page. Failure notes never carry metadata.
struct EnrichedFetch {
    content: Option<String>,
    title: Option<String>,
    published_date: Option<String>,
}

impl EnrichedFetch {
    /// A deterministic failure note stored as content — never a title source.
    fn note(note: String) -> Self {
        Self {
            content: Some(note),
            title: None,
            published_date: None,
        }
    }

    /// Specialist markdown: heading fallback only (specialist extractors
    /// return no structured metadata). Heading extraction runs before
    /// truncation so a tight `max_chars` cannot cut the title line in half.
    fn from_markdown(md: String, max_chars: usize) -> Self {
        let title = first_markdown_heading(&md);
        Self {
            content: Some(md.chars().take(max_chars).collect()),
            title,
            published_date: None,
        }
    }

    /// Generic provider page: provider metadata first, heading as fallback.
    fn from_page(page: FetchedPage, max_chars: usize) -> Self {
        let title = page
            .title
            .filter(|title| !is_junk_title(title))
            .or_else(|| first_markdown_heading(&page.content));
        Self {
            content: Some(page.content.chars().take(max_chars).collect()),
            title,
            published_date: page.published_date,
        }
    }
}

/// Headings longer than this are prose that happens to start with `#`, not a
/// plausible page title.
const MAX_HEADING_TITLE_CHARS: usize = 200;

/// First ATX heading (`# ` … `###### `) in the fetched markdown, as a
/// title-of-last-resort for sources whose provider returned none. Only the
/// first heading is considered — if it is junk or oversized, guessing a later
/// section heading would mislabel the page, so the title stays `None`.
fn first_markdown_heading(markdown: &str) -> Option<String> {
    let heading = markdown.lines().find_map(|line| {
        let trimmed = line.trim_start();
        let hashes = trimmed.chars().take_while(|&c| c == '#').count();
        if hashes == 0 || hashes > 6 {
            return None;
        }
        // ATX requires whitespace after the marker run; "#hashtag" is prose.
        let rest = &trimmed[hashes..];
        if !rest.starts_with(char::is_whitespace) {
            return None;
        }
        let text = rest.trim();
        (!text.is_empty()).then(|| text.to_string())
    })?;
    (heading.chars().count() <= MAX_HEADING_TITLE_CHARS && !is_junk_title(&heading))
        .then_some(heading)
}

/// Concurrently back-fill `Source.content` for the first `max_sources` sources
/// via the Phase 1 `resolve_content` pipeline; later sources stay
/// metadata-only (content = None) so a Grok response with dozens of citations
/// cannot blow up the payload — agents drill into them with `web_fetch`.
/// Bounded by `concurrency` (Semaphore) and the shared `deadline` (D-02:
/// per-source `timeout_at`, not an independent budget). Every enriched source
/// ends with `content = Some(..)` — real markdown (truncated to `max_chars`)
/// on success, or a deterministic `_Failed to retrieve: ..._` note on any
/// failure/timeout/invalid-url (D-05 within the inline window: never None,
/// never empty). Source order is preserved. While content is in hand, missing
/// `title`/`published_date` are back-filled from the fetched page (issue #21).
#[allow(clippy::too_many_arguments)]
async fn enrich_sources(
    sources: Vec<Source>,
    deadline: tokio::time::Instant,
    client: &reqwest::Client,
    router: &Arc<crate::sources::SourceRouter>,
    caps: crate::sources::SourceCaps,
    concurrency: usize,
    max_chars: usize,
    max_sources: usize,
    chain: Vec<SourceEntry>,
) -> Vec<Source> {
    let sem = Arc::new(tokio::sync::Semaphore::new(concurrency));
    let mut set: tokio::task::JoinSet<(usize, EnrichedFetch)> = tokio::task::JoinSet::new();

    for (idx, source) in sources.iter().enumerate().take(max_sources) {
        let permit = Arc::clone(&sem);
        let url_str = source.url.clone();
        let client = client.clone();
        let router = Arc::clone(router);
        let caps = caps.clone();
        let chain = chain.clone();

        set.spawn(async move {
            // acquire is micro-second scale for concurrency<=5; deadline
            // enforcement applies to the resolve_content call itself.
            let _permit = permit.acquire_owned().await.ok();
            let fetched = match url::Url::parse(&url_str) {
                Err(_) => EnrichedFetch::note(format!(
                    "_Failed to retrieve: invalid_url_\n\nSource: {url_str}"
                )),
                Ok(parsed) => {
                    let future = crate::sources::resolve_content(&client, &parsed, &router, &caps);
                    match tokio::time::timeout_at(deadline, future).await {
                        Ok(Ok((md, _kind))) => EnrichedFetch::from_markdown(md, max_chars),
                        // Specialist produced no content — either no specialist
                        // matched (generic URL) OR a matched specialist's API
                        // failed/rate-limited/rendered empty. Either way, mirror
                        // web_fetch and try the configured Tavily/Firecrawl generic
                        // fetch before giving up, so inline content still has page
                        // evidence when a source provider can fetch the URL (P1 +
                        // specialist-failure fallback). The original `reason` is
                        // surfaced only if the generic fetch also fails, and only
                        // when there was a chain to fail — see
                        // `enrichment_failure_reason`.
                        Ok(Err(reason)) => {
                            let generic = generic_source_fetch(&chain, &url_str);
                            match tokio::time::timeout_at(deadline, generic).await {
                                Ok(Ok(page)) => EnrichedFetch::from_page(page, max_chars),
                                Ok(Err(_)) => EnrichedFetch::note(format!(
                                    "_Failed to retrieve: {}_\n\nSource: {url_str}",
                                    enrichment_failure_reason(&chain, &reason)
                                )),
                                Err(_elapsed) => EnrichedFetch::note(format!(
                                    "_Failed to retrieve: timeout_\n\nSource: {url_str}"
                                )),
                            }
                        }
                        Err(_elapsed) => EnrichedFetch::note(format!(
                            "_Failed to retrieve: timeout_\n\nSource: {url_str}"
                        )),
                    }
                }
            };
            (idx, fetched)
        });
    }

    let mut results: Vec<(usize, EnrichedFetch)> = Vec::with_capacity(sources.len());
    while let Some(res) = set.join_next().await {
        if let Ok(pair) = res {
            results.push(pair);
        }
    }

    results.sort_by_key(|(idx, _)| *idx);
    let mut out = sources;
    for (idx, fetched) in results {
        let source = &mut out[idx];
        source.content = fetched.content;
        // Metadata backfill (issue #21): most Grok citations arrive as bare
        // URLs, so the fetched page is the only place a title/date can come
        // from. Fill only what upstream never provided — real upstream
        // metadata always wins, and un-enriched tail sources keep honest nulls.
        if source.title.is_none() {
            source.title = fetched.title;
        }
        if source.published_date.is_none() {
            source.published_date = fetched.published_date;
        }
    }
    out
}

/// Approximate serialized footprint of one source: every metadata field plus
/// inline content plus a fixed allowance for JSON keys/quotes/separators. The
/// budget must track what actually lands in the agent's context — a broad
/// query where Grok cites 50+ pages overflows on metadata alone, so counting
/// only inline content under-reports the payload.
fn source_weight(source: &Source) -> usize {
    const JSON_OVERHEAD: usize = 64;
    let opt_chars = |v: &Option<String>| v.as_deref().map(|s| s.chars().count()).unwrap_or(0);
    source.url.chars().count()
        + source.provider.chars().count()
        + opt_chars(&source.title)
        + opt_chars(&source.description)
        + opt_chars(&source.published_date)
        + source
            .content
            .as_deref()
            .map(|c| c.chars().count())
            .unwrap_or(0)
        + JSON_OVERHEAD
}

/// Trim the response from the TAIL until `answer_chars` plus the weighted
/// source list fits the `budget`. Head sources (Grok's own citations rank
/// first) survive intact. Two passes:
///
/// 1. Replace tail inline content with an actionable note naming `web_fetch`
///    and `get_sources` — the official MCP fetch server's "call again with
///    start_index" guidance, applied to sources.
/// 2. Still over budget (metadata overflow): drop whole tail sources from the
///    returned list, always keeping at least one.
///
/// The synthesized answer is never trimmed. Returns whether anything was
/// trimmed; callers always trim a clone so the session cache keeps everything.
fn apply_response_budget(
    answer_chars: usize,
    sources: &mut Vec<Source>,
    budget: usize,
    session_id: &str,
) -> bool {
    let content_chars = |s: &Source| s.content.as_deref().map(|c| c.chars().count()).unwrap_or(0);
    let mut total: usize = answer_chars + sources.iter().map(source_weight).sum::<usize>();
    if total <= budget {
        return false;
    }

    // Pass 1: swap tail inline content for recovery notes.
    for idx in (0..sources.len()).rev() {
        if total <= budget {
            break;
        }
        let len = content_chars(&sources[idx]);
        if len == 0 {
            continue;
        }
        let url = sources[idx].url.clone();
        let note = |verb: &str| {
            format!(
                "_[{verb}: response budget reached — full text via web_fetch(\"{url}\") or get_sources(session_id=\"{session_id}\", offset={idx}, limit=1)]_"
            )
        };
        let omit_note = note("inline content omitted");
        let omit_len = omit_note.chars().count();
        if len <= omit_len {
            // Replacing would not shrink the payload; leave it alone.
            continue;
        }
        let overshoot = total - budget;
        let trim_note = note("truncated");
        // "\n\n" separator + note must fit inside the chars we reclaim.
        let trim_overhead = trim_note.chars().count() + 2;
        if len > overshoot + trim_overhead {
            // Partial trim: keep a prefix so the head of the document survives.
            let keep = len - overshoot - trim_overhead;
            let prefix: String = sources[idx]
                .content
                .as_deref()
                .unwrap_or_default()
                .chars()
                .take(keep)
                .collect();
            sources[idx].content = Some(format!("{prefix}\n\n{trim_note}"));
            total -= overshoot;
        } else {
            sources[idx].content = Some(omit_note);
            total = total - len + omit_len;
        }
    }

    // Pass 2: metadata alone still over budget — cut whole tail sources.
    // They stay in the cache; get_sources(offset=..) pages through them.
    while total > budget && sources.len() > 1 {
        let dropped = sources.pop().expect("len > 1");
        total -= source_weight(&dropped);
    }

    true
}

fn with_provider(
    mut sources: Vec<Source>,
    provider: impl Into<std::borrow::Cow<'static, str>>,
) -> Vec<Source> {
    let provider = provider.into();
    for source in &mut sources {
        source.provider = provider.clone();
    }
    sources
}

struct Probe {
    ok: bool,
    /// Not probed because the provider is not configured.
    skipped: bool,
    /// Not probed because it was outside the requested `doctor` scope.
    not_checked: bool,
    detail: String,
}

impl Probe {
    fn ok(detail: impl Into<String>) -> Self {
        Self {
            ok: true,
            skipped: false,
            not_checked: false,
            detail: detail.into(),
        }
    }
    fn failed(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            skipped: false,
            not_checked: false,
            detail: detail.into(),
        }
    }
    fn skipped(detail: impl Into<String>) -> Self {
        Self {
            ok: false,
            skipped: true,
            not_checked: false,
            detail: detail.into(),
        }
    }
    fn not_checked() -> Self {
        Self {
            ok: false,
            skipped: false,
            not_checked: true,
            detail: "not checked (outside the requested provider)".to_string(),
        }
    }

    /// Health class, as in the DeepSeek plugin's `health_check`: `ok`,
    /// `not_configured`, `not_checked`, `auth_error`, `rate_limited`,
    /// `timeout`, or `error`.
    fn status(&self) -> &'static str {
        if self.ok {
            return "ok";
        }
        if self.not_checked {
            return "not_checked";
        }
        if self.skipped {
            return "not_configured";
        }
        classify_probe_error(&self.detail)
    }
}

/// Classify a provider error message for `doctor`.
fn classify_probe_error(detail: &str) -> &'static str {
    let lower = detail.to_ascii_lowercase();
    let has = |needle: &str| lower.contains(needle);
    if has("http 401")
        || has("http 403")
        || has("unauthori")
        || has("forbidden")
        || has("invalid api key")
        || has("invalid key")
    {
        "auth_error"
    } else if has("http 429")
        || has("http 432")
        || has("http 433")
        || has("rate limit")
        || has("rate-limit")
        || has("quota")
        || has("too many requests")
    {
        "rate_limited"
    } else if has("timed out") || has("timeout") {
        "timeout"
    } else {
        "error"
    }
}

async fn probe_source(provider: &dyn SourceProvider, sample_url: &str) -> Probe {
    // Use a short keyword search as a lightweight liveness signal.
    let filters = SearchFilters::default();
    match provider.search_sources("ping", 1, &filters).await {
        Ok(_) => Probe::ok(format!("reachable (sample probe via {sample_url} ok)")),
        Err(err) => Probe::failed(err.to_string()),
    }
}

struct FakeAiProvider;

#[async_trait]
impl AiProvider for FakeAiProvider {
    async fn search(&self, _request: &SearchRequest) -> Result<SearchResponse> {
        Ok(SearchResponse {
            content: "OpenAI published a verifiable update.".to_string(),
            sources: vec![
                Source::new("https://openai.com/news", "grok_responses").with_title("OpenAI News")
            ],
        })
    }
}

struct FakeSourceProvider;

#[async_trait]
impl SourceProvider for FakeSourceProvider {
    async fn search_sources(
        &self,
        _query: &str,
        max_results: usize,
        _filters: &SearchFilters,
    ) -> Result<Vec<Source>> {
        Ok((0..max_results)
            .map(|idx| {
                Source::new(format!("https://example.com/source-{idx}"), "tavily")
                    .with_title(format!("Source {idx}"))
            })
            .collect())
    }

    async fn fetch(&self, url: &str) -> Result<FetchedPage> {
        Ok(FetchedPage::text(format!("Fetched content from {url}")))
    }

    async fn map(&self, url: &str, max_results: usize) -> Result<Vec<Source>> {
        Ok((0..max_results)
            .map(|idx| Source::new(format!("{url}/page-{idx}"), "tavily"))
            .collect())
    }
}

#[cfg(test)]
mod transport_dispatch_tests {
    use super::*;
    use crate::config::Transport;

    #[test]
    fn service_constructs_for_chat_completions_protocol() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_PROTOCOL", "chat_completions"),
            ("GROK_SEARCH_URL", "https://example.com/v1"),
            ("GROK_SEARCH_API_KEY", "sk-fake"),
            ("GROK_SEARCH_MODEL", "grok-4.3-fast"),
            ("TAVILY_API_KEY", "fake-tavily"),
        ]);
        assert_eq!(config.transport, Transport::ChatCompletions);
        assert_eq!(resolve_default_model(&config), "grok-4.3-fast");
        let svc = SearchService::new(config).expect("service should build");
        drop(svc);
    }

    #[test]
    fn chat_completions_protocol_still_needs_the_grok_key() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_PROTOCOL", "chat_completions"),
            ("GROK_SEARCH_URL", "https://example.com/v1"),
        ]);
        let Err(err) = SearchService::new(config) else {
            panic!("chat_completions without a key must fail");
        };
        assert!(err.to_string().contains("GROK_SEARCH_API_KEY"), "{err}");
    }

    #[tokio::test]
    async fn doctor_reports_chat_completions_protocol_fields() {
        // doctor() must reflect the protocol the service dispatches with, and
        // the URL / model both protocols share.
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_PROTOCOL", "chat_completions"),
            ("GROK_SEARCH_URL", "https://compat.example/v1"),
            ("GROK_SEARCH_API_KEY", "sk-fake"),
            ("GROK_SEARCH_MODEL", "gpt-4o-mini"),
            // X-search is silently ignored on this transport — doctor must
            // report the effective behavior (false), not the raw env flag.
            ("GROK_SEARCH_X_SEARCH", "true"),
        ]);
        assert_eq!(config.transport, Transport::ChatCompletions);

        // Hand-build the service with fake AI to avoid any real HTTP from
        // probe_grok during doctor().
        let svc = SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(Vec::new()),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        };

        let report = svc.doctor().await;
        assert_eq!(report["provider"], "grok_chat_completions");
        assert_eq!(report["transport"], "grok_chat_completions");
        assert_eq!(report["grok"]["api_url"], "https://compat.example/v1");
        assert_eq!(report["grok"]["model"], "gpt-4o-mini");
        assert_eq!(report["grok"]["x_search_enabled"], false);
    }

    #[tokio::test]
    async fn doctor_still_reports_grok_responses_on_responses_transport() {
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "xai-fake"),
            ("GROK_SEARCH_MODEL", "grok-4-1-fast-reasoning"),
        ]);
        assert_eq!(config.transport, Transport::Responses);

        let svc = SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(Vec::new()),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        };

        let report = svc.doctor().await;
        assert_eq!(report["provider"], "grok_responses");
        assert_eq!(report["grok"]["model"], "grok-4-1-fast-reasoning");
    }

    #[tokio::test]
    async fn doctor_reports_github_token_status() {
        // With GITHUB_TOKEN set -> "set", and the raw value never leaks.
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "xai-fake"),
            ("GITHUB_TOKEN", "ghp_test"),
        ]);
        let svc = SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(Vec::new()),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        };
        let report = svc.doctor().await;
        assert_eq!(report["github_token"], "set");
        // No-leak: the full report must not contain the token value anywhere.
        assert!(
            !report.to_string().contains("ghp_test"),
            "token value leaked into doctor report: {report}"
        );

        // Without GITHUB_TOKEN -> "unset".
        let config_unset = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "xai-fake"),
        ]);
        let svc_unset = SearchService {
            default_model: resolve_default_model(&config_unset),
            config: config_unset,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(Vec::new()),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        };
        let report_unset = svc_unset.doctor().await;
        assert_eq!(report_unset["github_token"], "unset");
    }

    /// Build a service around a config whose file outcome the test dictates.
    fn service_for_config(config: Config) -> SearchService {
        SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(Vec::new()),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        }
    }

    #[tokio::test]
    async fn doctor_reports_a_rejected_config_file_and_why() {
        // The whole point: a typo used to void the entire file with nothing to
        // show for it but a stderr line the client swallowed, leaving the
        // operator to conclude their key "wasn't configured" (issue #35).
        let mut config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "xai-fake"),
        ]);
        config.config_file_path = Some(std::path::PathBuf::from("/tmp/does-not-matter.toml"));
        config.config_file_state =
            crate::config::ConfigFileState::Rejected("unknown field `tavly_api_key`".to_string());

        let report = service_for_config(config).doctor().await;

        assert_eq!(report["config_file"]["state"], "rejected");
        assert_eq!(
            report["config_file"]["path"], "/tmp/does-not-matter.toml",
            "the operator needs to know which file: {report}"
        );
        assert!(
            report["config_file"]["detail"]
                .as_str()
                .expect("a rejection carries its reason")
                .contains("tavly_api_key"),
            "the reason must name the offending key: {report}"
        );
    }

    #[tokio::test]
    async fn doctor_distinguishes_an_absent_config_file_from_a_rejected_one() {
        // Running on environment variables alone is normal, not a failure.
        let report = service_for_config(Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "xai-fake"),
        ]))
        .doctor()
        .await;

        assert_eq!(report["config_file"]["state"], "absent");
        assert!(
            report["config_file"]["detail"].is_null(),
            "nothing failed, so there is no reason to give: {report}"
        );
    }

    #[tokio::test]
    async fn doctor_reports_the_endpoints_that_will_actually_be_called() {
        // A report that echoes config the providers do not use would send the
        // operator to debug a URL nothing ever contacts.
        let config = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "xai-fake"),
            ("GROK_SEARCH_URL", "https://gateway.example"),
            ("EXA_API_URL", "https://exa.example"),
            ("TAVILY_API_URL", "https://tavily.example"),
        ]);
        let expected_grok = config.grok_api_url.clone();
        let expected_exa = config.exa_api_url.clone();
        let expected_tavily = config.tavily_api_url.clone();

        let report = service_for_config(config).doctor().await;

        assert_eq!(report["grok"]["api_url"], expected_grok);
        assert_eq!(report["exa"]["api_url"], expected_exa);
        assert_eq!(report["tavily"]["api_url"], expected_tavily);
    }

    #[tokio::test]
    async fn fake_with_router_constructs_and_clones() {
        let svc = SearchService::fake_with_router(
            Arc::new(FakeSourceProvider),
            None,
            crate::sources::SourceRouter::default(),
        );
        // SearchService derives Clone; storing Arc<SourceRouter> must preserve it.
        let _clone = svc.clone();
    }
}

#[cfg(test)]
mod enrich_tests {
    use super::*;
    use crate::sources::{SourceCaps, SourceExtractor, SourceRouter, SourceType};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;
    use url::Url;

    /// Always-matching extractor that records peak concurrency and returns a
    /// fixed body after a visibility sleep.
    struct CountingExtractor {
        peak: Arc<AtomicUsize>,
        current: Arc<AtomicUsize>,
        sleep_ms: u64,
    }
    #[async_trait]
    impl SourceExtractor for CountingExtractor {
        fn matches(&self, _url: &Url) -> bool {
            true
        }
        fn kind(&self) -> SourceType {
            SourceType::Wikipedia
        }
        async fn fetch_render(
            &self,
            _c: &reqwest::Client,
            _u: &Url,
            _caps: &SourceCaps,
        ) -> Result<String> {
            let n = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(n, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(self.sleep_ms)).await;
            self.current.fetch_sub(1, Ordering::SeqCst);
            Ok("content".to_string())
        }
    }

    /// URL-discriminating failure extractor: matches ONLY urls containing
    /// `fail_url_marker`, so a router can route one source here and the rest to
    /// CountingExtractor (true fault isolation).
    struct MarkerErrExtractor {
        fail_url_marker: String,
    }
    #[async_trait]
    impl SourceExtractor for MarkerErrExtractor {
        fn matches(&self, url: &Url) -> bool {
            url.as_str().contains(&self.fail_url_marker)
        }
        fn kind(&self) -> SourceType {
            SourceType::GithubIssue
        }
        async fn fetch_render(
            &self,
            _c: &reqwest::Client,
            _u: &Url,
            _caps: &SourceCaps,
        ) -> Result<String> {
            Err(crate::error::GrokSearchError::Provider(
                "always_fails".to_string(),
            ))
        }
    }

    /// Returns an oversized body to exercise the per-source char cap.
    struct OversizeExtractor {
        len: usize,
    }
    #[async_trait]
    impl SourceExtractor for OversizeExtractor {
        fn matches(&self, _url: &Url) -> bool {
            true
        }
        fn kind(&self) -> SourceType {
            SourceType::Wikipedia
        }
        async fn fetch_render(
            &self,
            _c: &reqwest::Client,
            _u: &Url,
            _caps: &SourceCaps,
        ) -> Result<String> {
            Ok("x".repeat(self.len))
        }
    }

    /// Hangs far past any test deadline — used to trigger the timeout note.
    struct HangingExtractor;
    #[async_trait]
    impl SourceExtractor for HangingExtractor {
        fn matches(&self, _url: &Url) -> bool {
            true
        }
        fn kind(&self) -> SourceType {
            SourceType::Wikipedia
        }
        async fn fetch_render(
            &self,
            _c: &reqwest::Client,
            _u: &Url,
            _caps: &SourceCaps,
        ) -> Result<String> {
            tokio::time::sleep(Duration::from_secs(3600)).await;
            Ok("never".to_string())
        }
    }

    /// Supplemental provider whose `search_sources` returns example.com sources
    /// but whose generic `fetch` always errors — used to exercise the
    /// "specialist failed AND generic fetch failed → note" path.
    struct SearchOkFetchErrProvider;
    #[async_trait]
    impl SourceProvider for SearchOkFetchErrProvider {
        async fn search_sources(
            &self,
            _query: &str,
            max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok((0..max_results)
                .map(|idx| Source::new(format!("https://example.com/source-{idx}"), "tavily"))
                .collect())
        }
        async fn fetch(&self, _url: &str) -> Result<FetchedPage> {
            Err(crate::error::GrokSearchError::Provider(
                "generic fetch unavailable".to_string(),
            ))
        }
        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    /// Generic `fetch` succeeds but yields whitespace-only content — exercises
    /// the "primary configured but empty" error path of `generic_source_fetch`.
    struct EmptyFetchProvider;
    #[async_trait]
    impl SourceProvider for EmptyFetchProvider {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
        async fn fetch(&self, _url: &str) -> Result<FetchedPage> {
            Ok(FetchedPage::text("  \n"))
        }
        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    /// Build a SearchService with fake AI + a caller-supplied supplemental
    /// provider, router, and config. Mirrors the doctor_* struct-literal tests.
    fn service_with_sources(
        config: Config,
        router: SourceRouter,
        sources: Option<Arc<dyn SourceProvider>>,
    ) -> SearchService {
        let source_slots = sources
            .map(|provider| {
                vec![SourceSlot::Active(SourceEntry {
                    spec: &TAVILY_SPEC,
                    provider,
                })]
            })
            .unwrap_or_default();
        SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: Some(Arc::new(FakeAiProvider)),
            ai_unavailable: None,
            source_slots: Arc::new(source_slots),
            cache: Arc::new(Mutex::new(SourceCache::new(64))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(30)),
            source_router: Arc::new(router),
        }
    }

    fn service_with(config: Config, router: SourceRouter) -> SearchService {
        service_with_sources(config, router, Some(Arc::new(FakeSourceProvider)))
    }

    fn enrich_config() -> Config {
        Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "fake-grok"),
            ("TAVILY_API_KEY", "fake-tavily"),
        ])
    }

    fn base_input() -> WebSearchInput {
        WebSearchInput {
            query: "q".to_string(),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn counting_extractor_self_test() {
        // Sanity: the helper itself records concurrency.
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let router = SourceRouter::with_extractors(vec![Box::new(CountingExtractor {
            peak: Arc::clone(&peak),
            current: Arc::clone(&current),
            sleep_ms: 5,
        })]);
        let svc = service_with(enrich_config(), router);
        let _ = svc.web_search(base_input()).await.expect("web_search");
        assert!(peak.load(Ordering::SeqCst) >= 1);
    }

    #[tokio::test]
    async fn web_search_inline_default_fills_content() {
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let router = SourceRouter::with_extractors(vec![Box::new(CountingExtractor {
            peak,
            current,
            sleep_ms: 0,
        })]);
        let svc = service_with(enrich_config(), router);
        let out = svc.web_search(base_input()).await.expect("web_search");

        assert!(!out.sources.is_empty());
        for s in &out.sources {
            let c = s.content.as_deref().unwrap_or("");
            assert!(!c.is_empty(), "every source must have non-empty content");
        }
    }

    #[tokio::test]
    async fn enrich_generic_url_uses_provider_fetch_fallback() {
        // No specialist matches the supplemental URLs → inline enrichment must
        // fall back to the configured source provider's generic fetch (mirroring
        // web_fetch), not emit a `_Failed to retrieve: no_specialist_match_`
        // note for ordinary search results (P1).
        let svc = service_with(enrich_config(), SourceRouter::default());
        let out = svc.web_search(base_input()).await.expect("web_search");

        assert!(!out.sources.is_empty());
        for s in &out.sources {
            let c = s.content.as_deref().unwrap_or("");
            assert!(
                c.starts_with("Fetched content from"),
                "generic source must use the provider fetch fallback, got: {c:?}"
            );
            assert!(
                !c.contains("no_specialist_match"),
                "must not leak the no_specialist_match note: {c:?}"
            );
        }
    }

    #[tokio::test]
    async fn enrich_names_the_missing_source_provider_when_the_chain_is_empty() {
        // With no source provider configured, nothing can fetch an ordinary
        // page — and that has nothing to do with specialist extractors, which
        // need no key and simply did not match this URL. Saying
        // "no_specialist_match" here sends the reader to debug the wrong thing;
        // it is what taught the reporter of #39 that specialists need keys.
        let svc = service_with_sources(enrich_config(), SourceRouter::default(), None);
        let out = svc.web_search(base_input()).await.expect("web_search");

        assert!(!out.sources.is_empty());
        for s in &out.sources {
            let c = s.content.as_deref().unwrap_or("");
            assert!(
                !c.contains("specialist"),
                "an empty chain is not a specialist problem: {c:?}"
            );
            assert!(
                c.contains("no source provider configured"),
                "the note must name what is actually missing: {c:?}"
            );
            assert!(
                c.contains("TAVILY_API_KEY"),
                "the note must say how to supply one: {c:?}"
            );
        }
    }

    #[tokio::test]
    async fn empty_chain_names_the_same_keys_to_web_fetch_and_to_enrichment() {
        // Two paths, one answer. They drifted apart before because each spelled
        // the missing config out by hand.
        let svc = service_with_sources(enrich_config(), SourceRouter::default(), None);

        let fetch_error = svc
            .web_fetch("https://example.com/plain", None)
            .await
            .expect_err("nothing can fetch a generic URL with an empty chain")
            .to_string();
        let out = svc.web_search(base_input()).await.expect("web_search");
        let note = out.sources[0].content.clone().unwrap_or_default();

        for key in [
            "TAVILY_API_KEY",
            "EXA_API_KEY",
            "TINYFISH_API_KEY",
            "FIRECRAWL_API_KEY",
        ] {
            assert!(
                fetch_error.contains(key),
                "web_fetch omits {key}: {fetch_error}"
            );
            assert!(note.contains(key), "enrichment omits {key}: {note}");
        }
    }

    #[tokio::test]
    async fn specialist_extractors_still_work_without_any_source_provider() {
        // The whole point of the distinction: specialists take no key, so an
        // empty chain must not stop them.
        let router = SourceRouter::with_extractors(vec![Box::new(CountingExtractor {
            peak: Arc::new(AtomicUsize::new(0)),
            current: Arc::new(AtomicUsize::new(0)),
            sleep_ms: 0,
        })]);
        let svc = service_with_sources(enrich_config(), router, None);
        let out = svc.web_search(base_input()).await.expect("web_search");

        assert!(!out.sources.is_empty());
        for s in &out.sources {
            let c = s.content.as_deref().unwrap_or("");
            assert!(
                !c.contains("Failed to retrieve"),
                "a specialist needs no source provider: {c:?}"
            );
            assert!(!c.is_empty(), "specialist content must land: {c:?}");
        }
    }

    #[tokio::test]
    async fn enrich_concurrency_is_bounded() {
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let router = SourceRouter::with_extractors(vec![Box::new(CountingExtractor {
            peak: Arc::clone(&peak),
            current: Arc::clone(&current),
            sleep_ms: 25, // wide enough window for overlap to register
        })]);
        let mut config = enrich_config();
        config.enrich_concurrency = 2;
        let svc = service_with(config, router);

        let _ = svc.web_search(base_input()).await.expect("web_search");
        // 4 sources, concurrency 2 → peak must never exceed 2.
        assert!(
            peak.load(Ordering::SeqCst) <= 2,
            "peak={}",
            peak.load(Ordering::SeqCst)
        );
    }

    #[tokio::test]
    async fn enrich_truncates_to_max_chars() {
        let router =
            SourceRouter::with_extractors(vec![Box::new(OversizeExtractor { len: 20_000 })]);
        let svc = service_with(enrich_config(), router); // default enrich_max_chars = 15000
        let out = svc.web_search(base_input()).await.expect("web_search");

        for s in &out.sources {
            let len = s.content.as_deref().map(|c| c.chars().count()).unwrap_or(0);
            assert!(len <= 15_000, "content len {len} exceeds cap");
            assert!(len > 0);
        }
    }

    #[tokio::test]
    async fn enrich_fault_isolation_one_fails_rest_ok() {
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let router = SourceRouter::with_extractors(vec![
            Box::new(MarkerErrExtractor {
                fail_url_marker: "openai.com".to_string(),
            }),
            Box::new(CountingExtractor {
                peak,
                current,
                sleep_ms: 0,
            }),
        ]);
        // Provider whose generic fetch ALSO fails, so the failing specialist
        // source genuinely falls through to the note (not the generic rescue).
        let svc = service_with_sources(
            enrich_config(),
            router,
            Some(Arc::new(SearchOkFetchErrProvider)),
        );
        let out = svc
            .web_search(base_input())
            .await
            .expect("web_search returns Ok despite one failure");

        let failed = out
            .sources
            .iter()
            .find(|s| s.url.contains("openai.com"))
            .expect("grok source present");
        let passed = out
            .sources
            .iter()
            .find(|s| s.url.contains("example.com"))
            .expect("supplemental source present");

        assert!(
            failed
                .content
                .as_deref()
                .unwrap_or("")
                .starts_with("_Failed to retrieve:"),
            "failing source must carry a failure note, got: {:?}",
            failed.content
        );
        let pc = passed.content.as_deref().unwrap_or("");
        assert!(
            !pc.is_empty() && !pc.starts_with("_Failed to retrieve:"),
            "passing source must carry real content, got: {pc:?}"
        );
    }

    #[tokio::test]
    async fn enrich_specialist_failure_rescued_by_generic_fetch() {
        // A matched specialist whose API errors must fall back to the configured
        // generic fetch (mirroring web_fetch), not store a failure note, when a
        // source provider can still fetch the URL.
        let router = SourceRouter::with_extractors(vec![Box::new(MarkerErrExtractor {
            fail_url_marker: "openai.com".to_string(),
        })]);
        let svc = service_with(enrich_config(), router); // FakeSourceProvider.fetch succeeds
        let out = svc.web_search(base_input()).await.expect("web_search");

        let failed = out
            .sources
            .iter()
            .find(|s| s.url.contains("openai.com"))
            .expect("grok source present");
        let content = failed.content.as_deref().unwrap_or("");
        assert!(
            content.starts_with("Fetched content from"),
            "specialist failure must be rescued by generic fetch, got: {content:?}"
        );
        assert!(
            !content.starts_with("_Failed to retrieve:"),
            "must not store a failure note when generic fetch succeeds: {content:?}"
        );
    }

    #[tokio::test]
    async fn generic_fetch_missing_config_only_when_no_provider_configured() {
        let err = generic_source_fetch(&[], "https://example.com")
            .await
            .expect_err("no providers must error");
        assert!(
            matches!(
                err,
                GrokSearchError::MissingConfig(
                    "TAVILY_API_KEY, EXA_API_KEY, TINYFISH_API_KEY or FIRECRAWL_API_KEY"
                )
            ),
            "expected MissingConfig, got: {err:?}"
        );
    }

    #[tokio::test]
    async fn generic_fetch_primary_error_surfaces_without_fallback() {
        // Regression: a configured primary whose fetch failed used to fall
        // through to MissingConfig("TAVILY_API_KEY or FIRECRAWL_API_KEY") even
        // though TAVILY_API_KEY was set, sending users to debug config instead
        // of the actual provider failure.
        let chain = vec![SourceEntry {
            spec: &TAVILY_SPEC,
            provider: Arc::new(SearchOkFetchErrProvider),
        }];
        let err = generic_source_fetch(&chain, "https://example.com")
            .await
            .expect_err("primary failure must error");
        match err {
            GrokSearchError::Provider(msg) => assert_eq!(msg, "generic fetch unavailable"),
            other => panic!("primary error must pass through unchanged, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn generic_fetch_primary_empty_content_reports_empty_without_fallback() {
        let url = "https://npmjs.com/package/grok-search-rs";
        let chain = vec![SourceEntry {
            spec: &TAVILY_SPEC,
            provider: Arc::new(EmptyFetchProvider),
        }];
        let err = generic_source_fetch(&chain, url)
            .await
            .expect_err("empty content must error");
        match err {
            GrokSearchError::Provider(msg) => assert!(
                msg.contains("empty content") && msg.contains(url),
                "message must name the empty result and url, got: {msg}"
            ),
            other => panic!("expected Provider empty-content error, got: {other:?}"),
        }
    }

    #[tokio::test]
    async fn generic_fetch_primary_failure_still_rescued_by_fallback() {
        let chain = vec![
            SourceEntry {
                spec: &TAVILY_SPEC,
                provider: Arc::new(SearchOkFetchErrProvider),
            },
            SourceEntry {
                spec: &FIRECRAWL_SPEC,
                provider: Arc::new(FakeSourceProvider),
            },
        ];
        let page = generic_source_fetch(&chain, "https://example.com")
            .await
            .expect("fallback must rescue primary failure");
        assert!(
            page.content.starts_with("Fetched content from"),
            "fallback content expected, got: {:?}",
            page.content
        );
    }

    #[tokio::test]
    async fn enrich_timeout_yields_note_not_error() {
        let router = SourceRouter::with_extractors(vec![Box::new(HangingExtractor)]);
        let mut config = enrich_config();
        config.timeout = Duration::from_millis(50); // deadline fires fast
        let svc = service_with(config, router);

        let out = svc
            .web_search(base_input())
            .await
            .expect("web_search returns Ok on timeout");
        for s in &out.sources {
            assert!(
                s.content.as_deref().unwrap_or("").contains("timeout"),
                "expected timeout note, got: {:?}",
                s.content
            );
        }
    }

    #[tokio::test]
    async fn include_content_false_omits_content_field() {
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let router = SourceRouter::with_extractors(vec![Box::new(CountingExtractor {
            peak,
            current,
            sleep_ms: 0,
        })]);
        let svc = service_with(enrich_config(), router);

        let mut input = base_input();
        input.include_content = Some(false);
        let out = svc.web_search(input).await.expect("web_search");

        for s in &out.sources {
            assert!(s.content.is_none());
            let value = serde_json::to_value(s).unwrap();
            assert!(
                value.get("content").is_none(),
                "JSON must omit the content key, not emit null"
            );
        }
    }

    #[tokio::test]
    async fn extra_sources_zero_suppresses_inline() {
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let router = SourceRouter::with_extractors(vec![Box::new(CountingExtractor {
            peak,
            current,
            sleep_ms: 0,
        })]);
        let svc = service_with(enrich_config(), router);

        let mut input = base_input();
        input.extra_sources = Some(0); // effective_extra_sources == 0 → dual gate suppresses enrich
        let out = svc.web_search(input).await.expect("web_search");

        for s in &out.sources {
            assert!(
                s.content.is_none(),
                "extra_sources=0 must keep the legacy no-content shape"
            );
        }
    }

    #[tokio::test]
    async fn get_sources_inherits_enriched_content() {
        let peak = Arc::new(AtomicUsize::new(0));
        let current = Arc::new(AtomicUsize::new(0));
        let router = SourceRouter::with_extractors(vec![Box::new(CountingExtractor {
            peak,
            current,
            sleep_ms: 0,
        })]);
        let svc = service_with(enrich_config(), router);

        let out = svc.web_search(base_input()).await.expect("web_search");
        let again = svc
            .get_sources(&out.session_id, 0, None)
            .await
            .expect("get_sources");

        assert_eq!(out.sources.len(), again.sources.len());
        for (a, b) in out.sources.iter().zip(again.sources.iter()) {
            assert_eq!(a.url, b.url);
            assert_eq!(
                a.content, b.content,
                "get_sources must reuse the cached enriched content"
            );
        }
    }

    /// Always-matching extractor returning markdown with a leading heading.
    struct HeadingExtractor {
        markdown: &'static str,
    }
    #[async_trait]
    impl SourceExtractor for HeadingExtractor {
        fn matches(&self, _url: &Url) -> bool {
            true
        }
        fn kind(&self) -> SourceType {
            SourceType::Wikipedia
        }
        async fn fetch_render(
            &self,
            _c: &reqwest::Client,
            _u: &Url,
            _caps: &SourceCaps,
        ) -> Result<String> {
            Ok(self.markdown.to_string())
        }
    }

    /// Generic provider whose fetch returns a page with full metadata.
    struct MetaFetchProvider;
    #[async_trait]
    impl SourceProvider for MetaFetchProvider {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
        async fn fetch(&self, _url: &str) -> Result<FetchedPage> {
            Ok(FetchedPage {
                content: "Page body.".to_string(),
                title: Some("Provider Title".to_string()),
                published_date: Some("2026-06-19T06:15:24-08:00".to_string()),
                ..FetchedPage::default()
            })
        }
        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    /// Drive enrich_sources directly with a permissive deadline/caps so the
    /// backfill rules can be asserted without a full web_search round-trip.
    async fn run_enrich(
        sources: Vec<Source>,
        router: SourceRouter,
        primary: Option<Arc<dyn SourceProvider>>,
        max_sources: usize,
    ) -> Vec<Source> {
        let chain = primary
            .map(|provider| {
                vec![SourceEntry {
                    spec: &TAVILY_SPEC,
                    provider,
                }]
            })
            .unwrap_or_default();
        enrich_sources(
            sources,
            tokio::time::Instant::now() + Duration::from_secs(30),
            &crate::providers::http::build_client(Duration::from_secs(5)),
            &Arc::new(router),
            SourceCaps {
                max_answers: 3,
                max_comments: 3,
            },
            4,
            15_000,
            max_sources,
            chain,
        )
        .await
    }

    fn bare_source(url: &str) -> Source {
        Source::new(url, "grok_responses")
    }

    #[tokio::test]
    async fn backfill_title_from_specialist_heading() {
        let router = SourceRouter::with_extractors(vec![Box::new(HeadingExtractor {
            markdown: "# Real Title\n\nBody text.",
        })]);
        let out = run_enrich(vec![bare_source("https://example.com/a")], router, None, 5).await;

        assert_eq!(out[0].title.as_deref(), Some("Real Title"));
        assert_eq!(out[0].published_date, None, "specialists carry no date");
    }

    #[tokio::test]
    async fn backfill_metadata_from_generic_provider() {
        // Empty router → no specialist matches → generic provider fetch path.
        let out = run_enrich(
            vec![bare_source("https://example.com/a")],
            SourceRouter::with_extractors(Vec::new()),
            Some(Arc::new(MetaFetchProvider)),
            5,
        )
        .await;

        assert_eq!(out[0].title.as_deref(), Some("Provider Title"));
        assert_eq!(
            out[0].published_date.as_deref(),
            Some("2026-06-19T06:15:24-08:00")
        );
    }

    #[tokio::test]
    async fn backfill_never_overwrites_upstream_metadata() {
        let source = bare_source("https://example.com/a")
            .with_title("Upstream Title")
            .with_published_date("2020-01-01");
        let out = run_enrich(
            vec![source],
            SourceRouter::with_extractors(Vec::new()),
            Some(Arc::new(MetaFetchProvider)),
            5,
        )
        .await;

        assert_eq!(out[0].title.as_deref(), Some("Upstream Title"));
        assert_eq!(out[0].published_date.as_deref(), Some("2020-01-01"));
    }

    #[tokio::test]
    async fn backfill_skipped_on_failed_fetch() {
        // No specialist + failing generic fetch → failure note, no metadata.
        let out = run_enrich(
            vec![bare_source("https://example.com/a")],
            SourceRouter::with_extractors(Vec::new()),
            Some(Arc::new(SearchOkFetchErrProvider)),
            5,
        )
        .await;

        assert!(out[0]
            .content
            .as_deref()
            .unwrap_or("")
            .starts_with("_Failed to retrieve:"));
        assert_eq!(out[0].title, None, "failure notes must not become titles");
        assert_eq!(out[0].published_date, None);
    }

    #[tokio::test]
    async fn backfill_rejects_junk_heading() {
        // First heading is a citation-index artifact — no guessing from later
        // section headings either.
        let router = SourceRouter::with_extractors(vec![Box::new(HeadingExtractor {
            markdown: "# 1\n\n## Real Section\n\nBody.",
        })]);
        let out = run_enrich(vec![bare_source("https://example.com/a")], router, None, 5).await;

        assert_eq!(out[0].title, None);
    }

    #[tokio::test]
    async fn backfill_leaves_tail_beyond_window_untouched() {
        let router = SourceRouter::with_extractors(vec![Box::new(HeadingExtractor {
            markdown: "# Real Title\n\nBody.",
        })]);
        let sources = vec![
            bare_source("https://example.com/a"),
            bare_source("https://example.com/b"),
        ];
        let out = run_enrich(sources, router, None, 1).await;

        assert_eq!(out[0].title.as_deref(), Some("Real Title"));
        assert_eq!(out[1].title, None, "un-enriched tail keeps honest nulls");
        assert_eq!(out[1].content, None);
    }

    #[test]
    fn first_markdown_heading_extracts_and_rejects() {
        assert_eq!(
            first_markdown_heading("# Title Line\nbody"),
            Some("Title Line".to_string())
        );
        assert_eq!(
            first_markdown_heading("prose first\n\n## Section Two\n"),
            Some("Section Two".to_string()),
            "first heading may appear after prose"
        );
        assert_eq!(first_markdown_heading("no headings at all"), None);
        assert_eq!(
            first_markdown_heading("#hashtag is prose"),
            None,
            "ATX heading requires whitespace after the marker"
        );
        assert_eq!(first_markdown_heading("####### seven hashes"), None);
        assert_eq!(
            first_markdown_heading("# 1\n\n# Real"),
            None,
            "junk first heading must not fall through to later ones"
        );
        let oversized = format!("# {}", "x".repeat(MAX_HEADING_TITLE_CHARS + 1));
        assert_eq!(first_markdown_heading(&oversized), None);
    }
}

#[cfg(test)]
mod request_scope_tests {
    use super::*;

    /// A long-lived template service, as the HTTP transport would hold one.
    fn template() -> SearchService {
        SearchService::new(Config::from_env_map([(
            "GROK_SEARCH_API_KEY",
            "xai-template",
        )]))
        .expect("template service builds")
    }

    #[test]
    fn with_config_rejects_oauth() {
        let svc = template();
        let cfg = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_AUTH_MODE", "oauth"),
        ]);
        assert!(
            svc.with_config(cfg).is_err(),
            "OAuth must be rejected on the per-request (HTTP) path"
        );
    }

    #[test]
    fn with_config_fails_closed_without_grok_key() {
        let svc = template();
        // No grok key -> construction fails rather than silently reusing the
        // template's key.
        let cfg = Config::from_env_map([("LLM_SEARCH_PROVIDER", "grok")]);
        assert!(
            svc.with_config(cfg).is_err(),
            "missing required key must fail closed, never fall back to the server key"
        );
    }

    #[test]
    fn with_config_accepts_per_request_key() {
        let svc = template();
        let cfg = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "xai-caller"),
            ("TAVILY_API_KEY", "tvly-caller"),
        ]);
        assert!(
            svc.with_config(cfg).is_ok(),
            "a caller-supplied key must build a request-scoped service"
        );
    }

    #[tokio::test]
    async fn cache_shared_within_tenant_isolated_across_tenants() {
        let svc = template(); // key "xai-template"
        let session = "abcdef012345";

        // Same-key request-scoped service shares cached sessions, so
        // get_sources continuation survives across requests.
        let same = svc
            .with_config(Config::from_env_map([(
                "GROK_SEARCH_API_KEY",
                "xai-template",
            )]))
            .expect("same-tenant scoped service");
        // Seed under the tenant-namespaced key, as web_search would.
        same.cache.lock().await.set(
            same.tenant_cache_key(session),
            Arc::new(Vec::<Source>::new()),
        );
        assert!(
            same.get_sources(session, 0, None).await.is_ok(),
            "same tenant must read its own cached session"
        );

        // A different caller key must NOT read that session.
        let other = svc
            .with_config(Config::from_env_map([
                ("LLM_SEARCH_PROVIDER", "grok"),
                ("GROK_SEARCH_API_KEY", "xai-other"),
            ]))
            .expect("other-tenant scoped service");
        assert!(
            other.get_sources(session, 0, None).await.is_err(),
            "a different tenant must not read another tenant's cached session"
        );
    }

    #[test]
    fn tenant_tag_namespaces_by_gateway() {
        // Same opaque key on two different gateways must NOT share a cache
        // namespace: with arbitrary public gateways, independent gateways can
        // issue/accept identical key strings for different callers.
        let on_xai = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "same-key"),
            ("GROK_SEARCH_URL", "https://api.x.ai"),
        ]);
        let on_other = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "same-key"),
            ("GROK_SEARCH_URL", "https://gateway.example"),
        ]);
        assert_ne!(
            tenant_tag(&on_xai),
            tenant_tag(&on_other),
            "gateway must be part of the tenant namespace"
        );
        // Same key + same gateway stays stable (continuation still works).
        let again = Config::from_env_map([
            ("LLM_SEARCH_PROVIDER", "grok"),
            ("GROK_SEARCH_API_KEY", "same-key"),
            ("GROK_SEARCH_URL", "https://api.x.ai"),
        ]);
        assert_eq!(tenant_tag(&on_xai), tenant_tag(&again));
    }
}

/// Router mode (`LLM_SEARCH_PROVIDER=brave`), `web_fetch` provenance, and
/// `provider_status`.
#[cfg(test)]
mod router_tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex as StdMutex;

    /// Scripted search provider: fixed results (or error), optional answer,
    /// call counting, and a record of the options and size it was asked for.
    struct Scripted {
        label: &'static str,
        urls: Vec<&'static str>,
        error: Option<&'static str>,
        answer: Option<&'static str>,
        calls: AtomicUsize,
        fetch_calls: AtomicUsize,
        seen_options: StdMutex<Option<SearchOptions>>,
        seen_max: AtomicUsize,
    }

    impl Scripted {
        fn build(
            label: &'static str,
            urls: Vec<&'static str>,
            error: Option<&'static str>,
            answer: Option<&'static str>,
        ) -> Arc<Self> {
            Arc::new(Self {
                label,
                urls,
                error,
                answer,
                calls: AtomicUsize::new(0),
                fetch_calls: AtomicUsize::new(0),
                seen_options: StdMutex::new(None),
                seen_max: AtomicUsize::new(0),
            })
        }
        fn ok(label: &'static str, urls: Vec<&'static str>) -> Arc<Self> {
            Self::build(label, urls, None, None)
        }
        fn failing(label: &'static str, error: &'static str) -> Arc<Self> {
            Self::build(label, vec![], Some(error), None)
        }
        fn with_answer(
            label: &'static str,
            urls: Vec<&'static str>,
            answer: &'static str,
        ) -> Arc<Self> {
            Self::build(label, urls, None, Some(answer))
        }
    }

    #[async_trait]
    impl SourceProvider for Scripted {
        async fn search_sources(
            &self,
            query: &str,
            max_results: usize,
            filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(self
                .search_detailed(query, max_results, filters, &SearchOptions::default())
                .await?
                .sources)
        }

        async fn search_detailed(
            &self,
            _query: &str,
            max_results: usize,
            _filters: &SearchFilters,
            options: &SearchOptions,
        ) -> Result<DetailedSearch> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.seen_max.store(max_results, Ordering::SeqCst);
            *self.seen_options.lock().unwrap() = Some(options.clone());
            if let Some(error) = self.error {
                return Err(GrokSearchError::Provider(error.to_string()));
            }
            Ok(DetailedSearch {
                answer: self.answer.map(str::to_string),
                sources: self
                    .urls
                    .iter()
                    .map(|url| {
                        Source::new(*url, self.label).with_title(format!("{} result", self.label))
                    })
                    .collect(),
                ..DetailedSearch::default()
            })
        }

        async fn fetch(&self, url: &str) -> Result<FetchedPage> {
            self.fetch_calls.fetch_add(1, Ordering::SeqCst);
            if url.contains("broken") {
                return Err(GrokSearchError::Provider(format!(
                    "{} cannot read {url}",
                    self.label
                )));
            }
            Ok(FetchedPage::text(format!("{} page for {url}", self.label)))
        }

        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    /// A provider with a batch endpoint: `fetch_batch` serves every URL not
    /// containing "broken"; single fetches always fail so a test can tell
    /// the batch path from the per-URL one.
    struct Batcher {
        batch_calls: StdMutex<Vec<Vec<String>>>,
    }

    #[async_trait]
    impl SourceProvider for Batcher {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }

        async fn fetch(&self, url: &str) -> Result<FetchedPage> {
            Err(GrokSearchError::Provider(format!("single fetch of {url}")))
        }

        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }

        async fn fetch_batch(
            &self,
            urls: &[String],
            _options: &FetchOptions,
            _deadline: tokio::time::Instant,
        ) -> Option<Result<crate::model::source::BatchFetch>> {
            self.batch_calls.lock().unwrap().push(urls.to_vec());
            Some(Ok(crate::model::source::BatchFetch {
                outcomes: urls
                    .iter()
                    .map(|url| {
                        if url.contains("broken") {
                            Err("Tavily extract failed: blocked".to_string())
                        } else {
                            Ok(FetchedPage {
                                request_id: Some("req-batch".to_string()),
                                ..FetchedPage::text(format!("batch page for {url}"))
                            })
                        }
                    })
                    .collect(),
                request_ids: vec!["req-batch".to_string()],
                usage: Some(serde_json::json!({ "credits": 1 })),
            }))
        }
    }

    /// A research provider that reports `pending` for the first `pending`
    /// polls and then the scripted final status.
    struct Researcher {
        pending: usize,
        final_status: &'static str,
        polls: AtomicUsize,
        starts: StdMutex<Vec<serde_json::Value>>,
    }

    impl Researcher {
        fn new(pending: usize, final_status: &'static str) -> Arc<Self> {
            Arc::new(Self {
                pending,
                final_status,
                polls: AtomicUsize::new(0),
                starts: StdMutex::new(Vec::new()),
            })
        }
    }

    #[async_trait]
    impl SourceProvider for Researcher {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }

        async fn fetch(&self, url: &str) -> Result<FetchedPage> {
            Err(GrokSearchError::Provider(format!("no fetch for {url}")))
        }

        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }

        async fn research_start(
            &self,
            body: &serde_json::Value,
        ) -> Option<Result<serde_json::Value>> {
            self.starts.lock().unwrap().push(body.clone());
            Some(Ok(
                serde_json::json!({ "request_id": "res-1", "status": "pending" }),
            ))
        }

        async fn research_get(&self, id: &str) -> Option<Result<serde_json::Value>> {
            let poll = self.polls.fetch_add(1, Ordering::SeqCst);
            if poll < self.pending {
                return Some(Ok(
                    serde_json::json!({ "request_id": id, "status": "in_progress" }),
                ));
            }
            Some(Ok(match self.final_status {
                "completed" => serde_json::json!({
                    "request_id": id,
                    "status": "completed",
                    "content": "Report body [1]",
                    "sources": [ { "title": "Src", "url": "https://src.example/", "favicon": "https://src.example/f.ico" } ]
                }),
                other => serde_json::json!({ "request_id": id, "status": other, "error": "quota" }),
            }))
        }
    }

    fn fast_timing(wait_ms: u64) -> ResearchTiming {
        ResearchTiming {
            initial: std::time::Duration::from_millis(1),
            max: std::time::Duration::from_millis(2),
            wait: std::time::Duration::from_millis(wait_ms),
        }
    }

    #[tokio::test]
    async fn research_polls_until_completed_and_sends_the_task_body() {
        let provider = Researcher::new(2, "completed");
        let mut research = serde_json::Map::new();
        research.insert("model".into(), serde_json::json!("mini"));
        let input = WebSearchInput {
            query: "Compare A and B on price and support".to_string(),
            mode: Some("research".to_string()),
            research,
            include_domains: vec!["sec.gov".to_string()],
            ..Default::default()
        };
        let outcome = run_research(provider.as_ref(), &input, &fast_timing(5_000))
            .await
            .expect("research");
        let ResearchOutcome::Completed {
            id,
            content,
            sources,
            ..
        } = outcome
        else {
            panic!("expected completion");
        };
        assert_eq!(id, "res-1");
        assert_eq!(content, "Report body [1]");
        assert_eq!(sources[0].url, "https://src.example/");
        assert_eq!(provider.polls.load(Ordering::SeqCst), 3);
        assert_eq!(
            provider.starts.lock().unwrap()[0],
            serde_json::json!({
                "input": "Compare A and B on price and support",
                "model": "mini",
                "include_domains": ["sec.gov"]
            })
        );
    }

    #[tokio::test]
    async fn research_reports_pending_past_the_wait_limit_and_resumes_by_id() {
        let provider = Researcher::new(usize::MAX, "completed");
        let input = WebSearchInput {
            query: "topic".to_string(),
            ..Default::default()
        };
        let outcome = run_research(provider.as_ref(), &input, &fast_timing(0))
            .await
            .expect("research");
        assert!(
            matches!(outcome, ResearchOutcome::Pending { ref id, ref status, .. } if id == "res-1" && status == "in_progress")
        );

        // Resume: no new task is started.
        let resumed = Researcher::new(0, "completed");
        let input = WebSearchInput {
            research_id: Some("res-1".to_string()),
            ..Default::default()
        };
        let outcome = run_research(resumed.as_ref(), &input, &fast_timing(5_000))
            .await
            .expect("resume");
        assert!(matches!(outcome, ResearchOutcome::Completed { .. }));
        assert!(resumed.starts.lock().unwrap().is_empty());

        let bad = WebSearchInput {
            research_id: Some("../etc".to_string()),
            ..Default::default()
        };
        assert!(run_research(resumed.as_ref(), &bad, &fast_timing(10))
            .await
            .is_err());
    }

    /// Polls fail with a transient upstream error after the task was queued.
    struct FlakyResearcher;

    #[async_trait]
    impl SourceProvider for FlakyResearcher {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }

        async fn fetch(&self, url: &str) -> Result<FetchedPage> {
            Err(GrokSearchError::Provider(format!("no fetch for {url}")))
        }

        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }

        async fn research_start(
            &self,
            _body: &serde_json::Value,
        ) -> Option<Result<serde_json::Value>> {
            Some(Ok(
                serde_json::json!({ "request_id": "res-7", "status": "pending" }),
            ))
        }

        async fn research_get(&self, _id: &str) -> Option<Result<serde_json::Value>> {
            Some(Err(GrokSearchError::Provider(
                "Tavily returned HTTP 502".to_string(),
            )))
        }
    }

    #[tokio::test]
    async fn research_poll_error_keeps_the_task_id() {
        let input = WebSearchInput {
            query: "topic".to_string(),
            ..Default::default()
        };
        let err = run_research(&FlakyResearcher, &input, &fast_timing(5_000))
            .await
            .err()
            .expect("poll error");
        let message = err.to_string();
        assert!(
            message.contains("HTTP 502") && message.contains("research_id=\"res-7\""),
            "{message}"
        );
    }

    #[tokio::test]
    async fn research_failure_is_a_provider_error() {
        let provider = Researcher::new(0, "failed");
        let input = WebSearchInput {
            query: "topic".to_string(),
            ..Default::default()
        };
        let err = run_research(provider.as_ref(), &input, &fast_timing(5_000))
            .await
            .err()
            .expect("failure");
        assert!(err.to_string().contains("res-1 failed: quota"), "{err}");
    }

    #[tokio::test]
    async fn research_mode_runs_on_any_engine_and_tells_the_caller_to_resume() {
        let provider = Researcher::new(usize::MAX, "completed");
        let mut svc = router_service(vec![(&TAVILY_SPEC, provider.clone())]);
        svc.config.research_max_wait = Some(std::time::Duration::from_millis(0));
        let out = svc
            .web_search(WebSearchInput {
                query: "topic".to_string(),
                mode: Some("research".to_string()),
                max_results: Some(3),
                ..Default::default()
            })
            .await
            .expect("research");
        assert_eq!(out.mode.as_deref(), Some("research"));
        assert_eq!(out.research_id.as_deref(), Some("res-1"));
        assert_eq!(out.status.as_deref(), Some("in_progress"));
        let notice = out.notice.expect("notice");
        assert!(notice.contains("research_id=\"res-1\""), "{notice}");
        assert!(out.warnings.iter().any(|w| w.contains("max_results")));

        let done = Researcher::new(0, "completed");
        let mut svc = router_service(vec![(&TAVILY_SPEC, done)]);
        svc.config.research_max_wait = Some(std::time::Duration::from_secs(5));
        let out = svc
            .web_search(WebSearchInput {
                query: "topic".to_string(),
                mode: Some("research".to_string()),
                ..Default::default()
            })
            .await
            .expect("research");
        assert_eq!(out.content, "Report body [1]");
        assert_eq!(out.status.as_deref(), Some("completed"));
        assert_eq!(out.source_log[0].source_id, "src_001");
        assert_eq!(
            out.sources[0].favicon.as_deref(),
            Some("https://src.example/f.ico")
        );
    }

    /// A site provider whose crawl returns two pages and one failure.
    struct Crawler;

    #[async_trait]
    impl SourceProvider for Crawler {
        async fn search_sources(
            &self,
            _query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }

        async fn fetch(&self, url: &str) -> Result<FetchedPage> {
            Err(GrokSearchError::Provider(format!("no fetch for {url}")))
        }

        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }

        async fn crawl(
            &self,
            _url: &str,
            _params: &serde_json::Map<String, serde_json::Value>,
        ) -> Option<Result<crate::model::source::CrawlResult>> {
            Some(Ok(crate::model::source::CrawlResult {
                base_url: Some("docs.example.com".to_string()),
                pages: vec![
                    (
                        "https://docs.example.com/a".to_string(),
                        FetchedPage::text("line a1\nline a2"),
                    ),
                    (
                        "https://docs.example.com/b".to_string(),
                        FetchedPage::text("line b1"),
                    ),
                ],
                failed: vec![(
                    "https://docs.example.com/c".to_string(),
                    "timeout".to_string(),
                )],
                request_id: Some("req-crawl".to_string()),
                usage: None,
            }))
        }
    }

    #[tokio::test]
    async fn web_crawl_writes_an_indexed_file_on_the_local_server() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut svc = router_service(vec![(&TAVILY_SPEC, Arc::new(Crawler))]);
        svc.config.output_dir = Some(dir.path().to_path_buf());
        let out = svc
            .web_crawl("https://docs.example.com/", &serde_json::Map::new())
            .await
            .expect("crawl");

        assert_eq!(out.pages_count, 2);
        assert!(out.sources.is_empty() && out.session_id.is_none());
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.request_id.as_deref(), Some("req-crawl"));
        let path = std::path::PathBuf::from(out.file_path.expect("file path"));
        assert!(path.starts_with(dir.path().join("crawl")));
        let text = std::fs::read_to_string(&path).expect("read file");
        let lines: Vec<&str> = text.lines().collect();
        // Each index line points at that page's section heading.
        for (idx, page) in out.pages.iter().enumerate() {
            let line = page.line.expect("line");
            assert_eq!(lines[line - 1], format!("## [{}] {}", idx + 1, page.url));
        }
        assert_eq!(out.pages[0].chars, "line a1\nline a2".chars().count());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[tokio::test]
    async fn web_crawl_returns_pages_inline_when_files_are_off() {
        let mut svc = router_service(vec![(&TAVILY_SPEC, Arc::new(Crawler))]);
        svc.config.write_crawl_files = false;
        let out = svc
            .web_crawl("https://docs.example.com/", &serde_json::Map::new())
            .await
            .expect("crawl");
        assert!(out.file_path.is_none());
        assert_eq!(out.sources.len(), 2);
        assert_eq!(out.sources[0].content.as_deref(), Some("line a1\nline a2"));
        let session = out.session_id.expect("session id");
        let cached = svc
            .get_sources(&session, 1, None)
            .await
            .expect("get_sources");
        assert_eq!(cached.total_sources, 2);
        assert_eq!(cached.sources[0].url, "https://docs.example.com/b");
    }

    #[tokio::test]
    async fn web_fetch_batch_sends_one_batch_and_falls_back_per_url_in_order() {
        let batcher = Arc::new(Batcher {
            batch_calls: StdMutex::new(Vec::new()),
        });
        let backup = Scripted::ok("exa", vec![]);
        let svc = router_service(vec![
            (&TAVILY_SPEC, batcher.clone()),
            (&EXA_SPEC, backup.clone()),
        ]);
        let urls = vec![
            "https://a.example/".to_string(),
            "https://broken.example/x".to_string(),
            "https://c.example/".to_string(),
        ];
        let out = svc
            .web_fetch_batch(&urls, None, &FetchOptions::default())
            .await
            .expect("batch");

        // One batch call with every generic URL, in input order.
        assert_eq!(*batcher.batch_calls.lock().unwrap(), vec![urls.clone()]);
        // The URL the batch could not read went to the next provider only.
        assert_eq!(backup.fetch_calls.load(Ordering::SeqCst), 1);
        assert!(out.failed.iter().any(|f| f.url == urls[1]
            && f.error.contains("Tavily: Tavily extract failed: blocked")
            && f.error.contains("exa cannot read")));
        let served: Vec<&str> = out.results.iter().map(|r| r.url.as_str()).collect();
        assert_eq!(served, vec![urls[0].as_str(), urls[2].as_str()]);
        assert_eq!(out.results[0].provider.as_deref(), Some("tavily"));
        assert_eq!(
            out.results[0].extras.request_id.as_deref(),
            Some("req-batch")
        );
        let ids: Vec<&str> = out
            .source_log
            .iter()
            .map(|e| e.source_id.as_str())
            .collect();
        assert_eq!(ids, vec!["src_001", "src_002", "src_003"]);
        assert_eq!(out.request_ids, vec!["req-batch".to_string()]);
        assert_eq!(out.usage, Some(serde_json::json!({ "credits": 1 })));
    }

    fn router_service(
        slots: Vec<(&'static ProviderSpec, Arc<dyn SourceProvider>)>,
    ) -> SearchService {
        router_service_with(&[], slots)
    }

    fn router_service_with(
        env: &[(&str, &str)],
        slots: Vec<(&'static ProviderSpec, Arc<dyn SourceProvider>)>,
    ) -> SearchService {
        let config = Config::from_env_map(
            [("LLM_SEARCH_PROVIDER", "brave")]
                .into_iter()
                .chain(env.iter().copied()),
        );
        SearchService {
            default_model: resolve_default_model(&config),
            config,
            ai: None,
            ai_unavailable: Some("no Grok configuration".to_string()),
            source_slots: Arc::new(
                slots
                    .into_iter()
                    .map(|(spec, provider)| SourceSlot::Active(SourceEntry { spec, provider }))
                    .collect(),
            ),
            cache: Arc::new(Mutex::new(SourceCache::new(16))),
            http_client: crate::providers::http::build_client(std::time::Duration::from_secs(5)),
            source_router: Arc::new(crate::sources::SourceRouter::default()),
        }
    }

    fn input(mode: Option<&str>) -> WebSearchInput {
        WebSearchInput {
            query: "router test".to_string(),
            mode: mode.map(str::to_string),
            ..Default::default()
        }
    }

    #[tokio::test]
    async fn quick_serves_from_brave_without_touching_tavily() {
        let brave = Scripted::ok("brave", vec!["https://a.example/", "https://b.example/"]);
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service(vec![
            (&BRAVE_SPEC, brave.clone()),
            (&TAVILY_SPEC, tavily.clone()),
        ]);

        let out = svc.web_search(input(None)).await.expect("search");

        assert_eq!(out.engine, "brave");
        assert_eq!(out.mode.as_deref(), Some("quick"));
        assert_eq!(out.search_provider, "brave");
        assert!(!out.fallback_used);
        assert!(out.notice.is_none());
        assert!(out.fallbacks.is_empty());
        assert_eq!(out.sources.len(), 2);
        assert_eq!(tavily.calls.load(Ordering::SeqCst), 0);
        assert_eq!(out.source_log.len(), 2);
        assert_eq!(out.source_log[0].source_id, "src_001");
        assert_eq!(out.source_log[1].rank, Some(2));
        assert_eq!(out.source_log[0].domain.as_deref(), Some("a.example"));
        assert_eq!(out.source_log[0].query.as_deref(), Some("router test"));
        // No content was requested: brave results stay snippets.
        assert!(out.sources.iter().all(|s| s.content.is_none()));
    }

    #[tokio::test]
    async fn quick_falls_back_to_tavily_and_discloses_it() {
        let brave = Scripted::failing("brave", "Brave returned HTTP 401 Unauthorized");
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service(vec![(&BRAVE_SPEC, brave), (&TAVILY_SPEC, tavily)]);

        let out = svc.web_search(input(Some("quick"))).await.expect("search");

        assert_eq!(out.search_provider, "tavily");
        assert!(out.fallback_used);
        assert_eq!(out.fallback_reason.as_deref(), Some("brave_error"));
        assert_eq!(out.fallbacks.len(), 1);
        assert_eq!(out.fallbacks[0].provider, "brave");
        assert!(out.fallbacks[0].error.contains("401"));
        let notice = out.notice.expect("notice");
        assert!(notice.contains("brave"), "{notice}");
        assert!(notice.contains("Disclose"), "{notice}");
    }

    #[tokio::test]
    async fn deep_prefers_tavily_with_advanced_options_and_answer() {
        let brave = Scripted::ok("brave", vec!["https://a.example/"]);
        let tavily =
            Scripted::with_answer("tavily", vec!["https://t.example/"], "Tavily says yes.");
        let svc = router_service(vec![
            (&BRAVE_SPEC, brave.clone()),
            (&TAVILY_SPEC, tavily.clone()),
        ]);

        let out = svc.web_search(input(Some("deep"))).await.expect("search");

        assert_eq!(out.search_provider, "tavily");
        assert_eq!(out.mode.as_deref(), Some("deep"));
        assert_eq!(out.answer.as_deref(), Some("Tavily says yes."));
        assert!(out.content.starts_with("Tavily says yes."));
        assert_eq!(brave.calls.load(Ordering::SeqCst), 0);
        let options = tavily
            .seen_options
            .lock()
            .unwrap()
            .clone()
            .expect("options");
        assert_eq!(options.depth, crate::model::search::SearchDepth::Advanced);
        assert!(options.include_answer);
        assert!(options.include_raw_content);
        assert_eq!(options.raw_content_max_chars, ROUTER_RAW_CONTENT_CHARS);
    }

    #[tokio::test]
    async fn quick_keeps_tavily_basic() {
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service(vec![(&TAVILY_SPEC, tavily.clone())]);

        svc.web_search(input(None)).await.expect("search");

        let options = tavily
            .seen_options
            .lock()
            .unwrap()
            .clone()
            .expect("options");
        assert_eq!(options, SearchOptions::default());
    }

    #[tokio::test]
    async fn tavily_only_params_reach_tavily_and_warn_when_brave_serves() {
        let mut tavily_params = serde_json::Map::new();
        tavily_params.insert("topic".into(), serde_json::json!("news"));
        let hints = crate::model::search::SearchHints {
            country: Some("TW".to_string()),
            ..Default::default()
        };
        let request = WebSearchInput {
            tavily: tavily_params.clone(),
            hints: hints.clone(),
            ..input(None)
        };

        // quick: Brave serves, the Tavily-only field is reported as ignored.
        let brave = Scripted::ok("brave", vec!["https://a.example/"]);
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service(vec![(&BRAVE_SPEC, brave.clone()), (&TAVILY_SPEC, tavily)]);
        let out = svc.web_search(request.clone()).await.expect("search");
        assert_eq!(out.search_provider, "brave");
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("Tavily-only") && w.contains("topic")),
            "{:?}",
            out.warnings
        );
        let brave_options = brave.seen_options.lock().unwrap().clone().expect("options");
        assert_eq!(brave_options.hints, hints);
        assert!(brave_options.tavily.is_empty());

        // deep: Tavily serves with the field merged in and no warning.
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service(vec![(&TAVILY_SPEC, tavily.clone())]);
        let out = svc
            .web_search(WebSearchInput {
                mode: Some("deep".to_string()),
                ..request
            })
            .await
            .expect("search");
        let options = tavily
            .seen_options
            .lock()
            .unwrap()
            .clone()
            .expect("options");
        assert_eq!(options.tavily, tavily_params);
        assert_eq!(options.depth, crate::model::search::SearchDepth::Advanced);
        // country + topic=news: Tavily drops the country and says so.
        assert!(
            out.warnings
                .iter()
                .any(|w| w.contains("country is ignored by Tavily")),
            "{:?}",
            out.warnings
        );
        assert!(!out.warnings.iter().any(|w| w.contains("Tavily-only")));
    }

    #[tokio::test]
    async fn end_date_skips_providers_that_cannot_bound_it() {
        let tinyfish = Scripted::ok("tinyfish", vec!["https://f.example/"]);
        let exa = Scripted::ok("exa", vec!["https://e.example/"]);
        let svc = router_service(vec![(&TINYFISH_SPEC, tinyfish.clone()), (&EXA_SPEC, exa)]);
        let out = svc
            .web_search(WebSearchInput {
                end_date: Some("2026-01-31".to_string()),
                ..input(None)
            })
            .await
            .expect("search");
        assert_eq!(out.search_provider, "exa");
        assert_eq!(tinyfish.calls.load(Ordering::SeqCst), 0);
        assert!(out
            .fallbacks
            .iter()
            .any(|f| f.provider == "tinyfish" && f.error.contains("skipped")));
    }

    #[tokio::test]
    async fn verify_runs_both_and_merges_by_url() {
        let brave = Scripted::ok(
            "brave",
            vec!["https://a.example/", "https://shared.example/"],
        );
        let tavily = Scripted::ok(
            "tavily",
            vec!["https://shared.example/", "https://t.example/"],
        );
        let svc = router_service(vec![
            (&BRAVE_SPEC, brave.clone()),
            (&TAVILY_SPEC, tavily.clone()),
        ]);

        let out = svc.web_search(input(Some("verify"))).await.expect("search");

        assert_eq!(out.search_provider, "mixed");
        assert_eq!(brave.calls.load(Ordering::SeqCst), 1);
        assert_eq!(tavily.calls.load(Ordering::SeqCst), 1);
        let urls: Vec<&str> = out.sources.iter().map(|s| s.url.as_str()).collect();
        assert_eq!(
            urls,
            vec![
                "https://a.example/",
                "https://shared.example/",
                "https://t.example/"
            ]
        );
        // The shared URL keeps the provider that returned it first.
        assert_eq!(out.sources[1].provider, "brave");
        assert!(out.notice.is_none());
    }

    #[tokio::test]
    async fn verify_reports_the_side_that_failed() {
        let brave = Scripted::failing("brave", "Brave returned HTTP 429 Too Many Requests");
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service(vec![(&BRAVE_SPEC, brave), (&TAVILY_SPEC, tavily)]);

        let out = svc.web_search(input(Some("verify"))).await.expect("search");

        assert_eq!(out.search_provider, "tavily");
        assert_eq!(out.fallbacks.len(), 1);
        assert_eq!(out.fallbacks[0].provider, "brave");
        assert!(out.notice.is_some());
    }

    #[tokio::test]
    async fn rest_of_chain_serves_when_brave_and_tavily_fail() {
        let brave = Scripted::failing("brave", "down");
        let tavily = Scripted::failing("tavily", "down");
        let exa = Scripted::ok("exa", vec!["https://e.example/"]);
        let svc = router_service(vec![
            (&TAVILY_SPEC, tavily),
            (&EXA_SPEC, exa),
            (&BRAVE_SPEC, brave),
        ]);

        let out = svc.web_search(input(Some("verify"))).await.expect("search");

        assert_eq!(out.search_provider, "exa");
        let tried: Vec<&str> = out.fallbacks.iter().map(|f| f.provider.as_str()).collect();
        assert_eq!(tried, vec!["brave", "tavily"]);
    }

    #[tokio::test]
    async fn empty_results_count_as_a_fallback() {
        let brave = Scripted::ok("brave", vec![]);
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service(vec![(&BRAVE_SPEC, brave), (&TAVILY_SPEC, tavily)]);

        let out = svc.web_search(input(None)).await.expect("search");

        assert_eq!(out.fallback_reason.as_deref(), Some("brave_no_results"));
        assert_eq!(out.fallbacks[0].error, "no results");
    }

    #[tokio::test]
    async fn all_failures_name_every_provider() {
        let brave = Scripted::failing("brave", "brave broke");
        let tavily = Scripted::failing("tavily", "tavily broke");
        let svc = router_service(vec![(&BRAVE_SPEC, brave), (&TAVILY_SPEC, tavily)]);

        let err = svc.web_search(input(None)).await.expect_err("must fail");
        let message = err.to_string();
        assert!(message.contains("brave: "), "{message}");
        assert!(message.contains("tavily: "), "{message}");
    }

    #[tokio::test]
    async fn no_search_provider_is_a_config_error() {
        let svc = router_service(vec![]);
        let err = svc.web_search(input(None)).await.expect_err("must fail");
        assert!(matches!(err, GrokSearchError::MissingConfig(_)), "{err}");
        // Any one search-capable provider is enough, so the message names all five.
        let message = err.to_string();
        for key in [
            "BRAVE_API_KEY",
            "TAVILY_API_KEY",
            "EXA_API_KEY",
            "TINYFISH_API_KEY",
            "FIRECRAWL_API_KEY",
        ] {
            assert!(message.contains(key), "{message}");
        }
    }

    #[tokio::test]
    async fn max_results_is_clamped_to_ten() {
        let brave = Scripted::ok("brave", vec!["https://a.example/"]);
        let svc = router_service(vec![(&BRAVE_SPEC, brave.clone())]);

        let mut request = input(None);
        request.max_results = Some(50);
        svc.web_search(request).await.expect("search");
        assert_eq!(brave.seen_max.load(Ordering::SeqCst), ROUTER_MAX_RESULTS);

        svc.web_search(input(None)).await.expect("search");
        assert_eq!(
            brave.seen_max.load(Ordering::SeqCst),
            ROUTER_DEFAULT_RESULTS
        );
    }

    #[tokio::test]
    async fn unknown_mode_is_rejected() {
        let brave = Scripted::ok("brave", vec!["https://a.example/"]);
        let svc = router_service(vec![(&BRAVE_SPEC, brave)]);
        let err = svc
            .web_search(input(Some("fast")))
            .await
            .expect_err("must fail");
        assert!(matches!(err, GrokSearchError::InvalidParams(_)), "{err}");
    }

    #[tokio::test]
    async fn filtered_requests_skip_filter_blind_providers() {
        // Brave unconfigured: quick runs Tavily (down), then the rest of the
        // chain, where Firecrawl cannot honor a recency filter.
        let firecrawl = Scripted::ok("firecrawl", vec!["https://f.example/"]);
        let svc = router_service(vec![
            (&TAVILY_SPEC, Scripted::failing("tavily", "down")),
            (&FIRECRAWL_SPEC, firecrawl.clone()),
        ]);
        let mut request = input(None);
        request.recency_days = Some(3);
        let err = svc
            .web_search(request)
            .await
            .expect_err("nothing can serve");
        assert!(err.to_string().contains("firecrawl: skipped"), "{err}");
        assert_eq!(firecrawl.calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn grok_mode_warns_about_router_only_parameters() {
        let svc = SearchService::fake_with_sources();
        let mut request = input(Some("deep"));
        request.max_results = Some(3);
        request.include_content = Some(false);
        let out = svc.web_search(request).await.expect("search");
        assert_eq!(out.engine, "grok");
        assert!(out.mode.is_none());
        assert_eq!(out.warnings.len(), 2, "{:?}", out.warnings);
        assert_eq!(out.source_log.len(), out.sources.len());
    }

    #[tokio::test]
    async fn web_fetch_reports_provider_hash_and_source_log() {
        let tavily = Scripted::ok("tavily", vec![]);
        let svc = router_service(vec![(&TAVILY_SPEC, tavily)]);

        let out = svc
            .web_fetch("https://docs.example/page", None)
            .await
            .expect("fetch");

        assert_eq!(out.provider.as_deref(), Some("tavily"));
        assert_eq!(out.domain.as_deref(), Some("docs.example"));
        assert_eq!(out.content_hash, sha256_hex(&out.content));
        assert_eq!(out.content_hash.len(), 64);
        assert!(out.extracted_at.ends_with('Z'));
        assert_eq!(out.source_log.len(), 1);
        assert_eq!(
            out.source_log[0].content_hash.as_deref(),
            Some(out.content_hash.as_str())
        );
    }

    #[tokio::test]
    async fn web_fetch_never_routes_to_search_only_brave() {
        let brave = Scripted::ok("brave", vec![]);
        let tavily = Scripted::ok("tavily", vec![]);
        let svc = router_service(vec![(&BRAVE_SPEC, brave.clone()), (&TAVILY_SPEC, tavily)]);

        let out = svc
            .web_fetch("https://docs.example/page", None)
            .await
            .expect("fetch");

        assert_eq!(out.provider.as_deref(), Some("tavily"));
        assert_eq!(brave.fetch_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn batch_fetch_isolates_failures_and_shares_the_budget() {
        let tavily = Scripted::ok("tavily", vec![]);
        let svc = router_service(vec![(&TAVILY_SPEC, tavily)]);
        let urls = vec![
            "https://one.example/a".to_string(),
            "https://broken.example/b".to_string(),
            "https://three.example/c".to_string(),
        ];

        let out = svc
            .web_fetch_batch(&urls, Some(40), &FetchOptions::default())
            .await
            .expect("batch");

        assert_eq!(out.results.len(), 2);
        assert_eq!(out.failed.len(), 1);
        assert_eq!(out.failed[0].url, "https://broken.example/b");
        let total: usize = out.results.iter().map(|r| r.content.chars().count()).sum();
        assert!(total <= 40, "budget exceeded: {total}");
        assert!(out.truncated);
        let ids: Vec<(&str, &str)> = out
            .source_log
            .iter()
            .map(|e| (e.source_id.as_str(), e.status.as_str()))
            .collect();
        assert_eq!(
            ids,
            vec![
                ("src_001", "success"),
                ("src_002", "failed"),
                ("src_003", "success")
            ]
        );
        // Each result's hash matches its (possibly cut) content.
        for result in &out.results {
            assert_eq!(result.content_hash, sha256_hex(&result.content));
            assert!(result.source_log.is_empty());
        }
    }

    #[test]
    fn provider_status_reports_order_and_no_secrets() {
        let secret = "brave-secret-key-0123456789";
        let config = Config::from_env_map([
            ("BRAVE_API_KEY", secret),
            ("TAVILY_API_KEY", "tvly-secret-key-0123456789"),
        ]);
        let svc = SearchService::new(config).expect("router mode needs no Grok");
        let status = svc.provider_status();
        let text = status.to_string();
        assert!(!text.contains(secret), "{text}");
        assert!(!text.contains("tvly-secret"), "{text}");
        assert_eq!(status["engine"], "brave");
        assert_eq!(status["providers"]["brave"]["configured"], true);
        assert_eq!(status["providers"]["grok"]["configured"], false);
        assert_eq!(status["providers"]["grok"]["required"], false);
        assert_eq!(
            status["web_search_order"]["quick"],
            serde_json::json!(["brave", "tavily"])
        );
        assert_eq!(
            status["web_search_order"]["deep"],
            serde_json::json!(["tavily", "brave"])
        );
        assert_eq!(status["web_fetch_order"], serde_json::json!(["tavily"]));
    }

    #[test]
    fn mode_order_defaults_and_overrides() {
        // Active providers in chain order.
        let active = ["tavily", "exa", "tinyfish", "firecrawl", "brave"];
        let default = Config::from_env_map([("LLM_SEARCH_PROVIDER", "brave")]);
        assert_eq!(
            router_mode_order(&active, RouterMode::Quick, &default),
            ["brave", "tavily", "exa", "tinyfish", "firecrawl"]
        );
        assert_eq!(
            router_mode_order(&active, RouterMode::Deep, &default),
            ["tavily", "brave", "exa", "tinyfish", "firecrawl"]
        );

        let custom = Config::from_env_map([
            ("LLM_SEARCH_QUICK_ORDER", "[exa, tavily]"),
            ("LLM_SEARCH_DEEP_ORDER", "brave,firecrawl"),
        ]);
        // Listed providers first; unlisted keep the default order behind them.
        assert_eq!(
            router_mode_order(&active, RouterMode::Quick, &custom),
            ["exa", "tavily", "brave", "tinyfish", "firecrawl"]
        );
        assert_eq!(
            router_mode_order(&active, RouterMode::Deep, &custom),
            ["brave", "firecrawl", "tavily", "exa", "tinyfish"]
        );
        // Listed providers that are not configured are skipped.
        assert_eq!(
            router_mode_order(&["brave", "tavily"], RouterMode::Quick, &custom),
            ["tavily", "brave"]
        );
    }

    #[test]
    fn unknown_name_in_mode_order_is_rejected() {
        let config = Config::from_env_map([("LLM_SEARCH_DEEP_ORDER", "tavily,serpapi")]);
        let err = validate_source_providers(&config).expect_err("must reject");
        assert!(err.to_string().contains("LLM_SEARCH_DEEP_ORDER"), "{err}");
        assert!(err.to_string().contains("serpapi"), "{err}");
    }

    #[tokio::test]
    async fn quick_order_env_puts_tavily_first() {
        let brave = Scripted::ok("brave", vec!["https://a.example/"]);
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service_with(
            &[("LLM_SEARCH_QUICK_ORDER", "tavily")],
            vec![(&BRAVE_SPEC, brave.clone()), (&TAVILY_SPEC, tavily.clone())],
        );

        let out = svc.web_search(input(None)).await.expect("search");

        assert_eq!(out.search_provider, "tavily");
        assert_eq!(brave.calls.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn provider_status_reports_configured_mode_order() {
        let config = Config::from_env_map([
            ("BRAVE_API_KEY", "brave-key"),
            ("TAVILY_API_KEY", "tvly-key"),
            ("LLM_SEARCH_QUICK_ORDER", "tavily"),
            ("LLM_SEARCH_DEEP_ORDER", "[brave]"),
        ]);
        let svc = SearchService::new(config).expect("router mode needs no Grok");
        let status = svc.provider_status();
        assert_eq!(
            status["web_search_order"]["quick"],
            serde_json::json!(["tavily", "brave"])
        );
        assert_eq!(
            status["web_search_order"]["deep"],
            serde_json::json!(["brave", "tavily"])
        );
    }

    #[test]
    fn probe_errors_are_classified() {
        assert_eq!(
            classify_probe_error("Brave returned HTTP 401 Unauthorized: {}"),
            "auth_error"
        );
        assert_eq!(
            classify_probe_error("Tavily returned HTTP 432: plan limit"),
            "rate_limited"
        );
        assert_eq!(
            classify_probe_error("Brave request timed out: ..."),
            "timeout"
        );
        assert_eq!(classify_probe_error("connection refused"), "error");
        assert_eq!(
            Probe::skipped("BRAVE_API_KEY not configured").status(),
            "not_configured"
        );
        assert_eq!(Probe::not_checked().status(), "not_checked");
    }

    #[tokio::test]
    async fn doctor_scope_skips_other_providers() {
        let brave = Scripted::ok("brave", vec!["https://a.example/"]);
        let tavily = Scripted::ok("tavily", vec!["https://t.example/"]);
        let svc = router_service(vec![
            (&BRAVE_SPEC, brave.clone()),
            (&TAVILY_SPEC, tavily.clone()),
        ]);

        let report = svc.doctor_scoped(Some("brave")).await.expect("doctor");

        assert_eq!(report["brave"]["status"], "ok");
        assert_eq!(report["tavily"]["status"], "not_checked");
        assert_eq!(report["grok"]["status"], "not_checked");
        assert_eq!(tavily.calls.load(Ordering::SeqCst), 0);
        assert!(svc.doctor_scoped(Some("bing")).await.is_err());
    }
}
