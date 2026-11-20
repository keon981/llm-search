use crate::error::{GrokSearchError, Result};
use crate::model::search::{ExtractFormat, FetchOptions, SearchDepth};
use crate::model::tool::WebSearchInput;
use crate::redact::{redact_text, redact_value};
use crate::service::SearchService;

/// Appended to `web_search` failures, as in the DeepSeek plugin: the calling
/// agent still has its own search tool and should say this one failed.
const NATIVE_SEARCH_FALLBACK_HINT: &str = "If llm-search keeps failing, fall back to the client's built-in web search (Claude Code: WebSearch) and tell the user that the llm-search MCP failed.";

/// Most URLs one `web_fetch` call accepts through `urls`.
/// Matches Tavily Extract's per-call limit, so one batch call covers them all.
const MAX_FETCH_URLS: usize = 20;
use serde_json::{json, Value};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::task::JoinSet;

/// Requests handled at once. Unbounded fan-out would let one runaway client
/// drive the upstream into the rate limits this concurrency exists to relieve.
/// Excess requests wait rather than being refused: queueing is the right answer
/// for a single local client, whereas the multi-tenant HTTP transport has its
/// own overload response.
const MAX_IN_FLIGHT: usize = 8;

/// Bind the request loop to the process's stdin/stdout. Binding is all this
/// does; every decision about what a message means lives in [`serve`].
pub async fn run_stdio(service: SearchService) -> anyhow::Result<()> {
    serve(service, tokio::io::stdin(), tokio::io::stdout()).await
}

/// The stdio transport's request loop, taking its transport as parameters so
/// it can be driven over in-memory buffers: one JSON-RPC message per input
/// line, one response line per message that carries an `id`.
pub(crate) async fn serve<R, W>(
    service: SearchService,
    reader: R,
    mut writer: W,
) -> anyhow::Result<()>
where
    R: AsyncRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut in_flight: JoinSet<Option<Value>> = JoinSet::new();
    let mut reading = true;

    // Only this loop ever touches the writer, so serialization of responses is
    // structural: two handlers cannot interleave halves of a line no matter how
    // they overlap. Responses leave in completion order, which JSON-RPC allows
    // because every one carries the `id` it answers.
    while reading || !in_flight.is_empty() {
        tokio::select! {
            // Reading pauses at the cap. That is what makes a burst queue here
            // instead of arriving at the upstream all at once.
            line = lines.next_line(), if reading && in_flight.len() < MAX_IN_FLIGHT => {
                match line? {
                    None => reading = false,
                    Some(line) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        match serde_json::from_str::<Value>(&line) {
                            Ok(request) => {
                                let service = service.clone();
                                in_flight
                                    .spawn(async move { handle_message(&service, request).await });
                            }
                            Err(err) => {
                                let response = error_response(
                                    Value::Null,
                                    -32700,
                                    format!("parse error: {err}"),
                                );
                                write_line(&mut writer, &response).await?;
                            }
                        }
                    }
                }
            }
            Some(finished) = in_flight.join_next() => {
                if let Some(response) = finished? {
                    write_line(&mut writer, &response).await?;
                }
            }
        }
    }

    Ok(())
}

/// One response per line, written whole.
async fn write_line<W>(writer: &mut W, response: &Value) -> anyhow::Result<()>
where
    W: AsyncWrite + Unpin,
{
    writer.write_all(response.to_string().as_bytes()).await?;
    writer.write_all(b"\n").await?;
    Ok(())
}

/// Server-level usage guide returned as `instructions` in the `initialize`
/// result (defined in the 2025-11-25 schema; the 2024-11-05 schema has no such
/// field). Tool descriptions say what each tool does; this says which tool to
/// pick and how to search well. Kept ASCII and within 2,048 UTF-16 code units:
/// Claude Code truncates server instructions at 4,096 units today and did so
/// at 2,048 in older releases, so the smaller cap is safe on both. The test
/// `instructions_fit_client_budget_and_name_every_tool` enforces the cap.
pub(crate) const SERVER_INSTRUCTIONS: &str = "\
llm-search: live web search and page reading with source provenance.\n\
\n\
Pick the tool:\n\
- Have a URL -> web_fetch (url, or urls for up to 20 pages). Do not search for a page you can fetch.\n\
- Need sources or current facts -> web_search.\n\
- Know the site but not the page -> web_map, then web_fetch the right URL.\n\
- Need many pages of one site section -> web_crawl (local runs write a file; read it in parts).\n\
- User asks for research or a report -> web_search mode=research with a full task description (never on your own; it is slow and costly).\n\
- Re-read sources from an earlier web_search -> get_sources with its session_id (no new search or quota).\n\
- Setup check or repeated failures -> provider_status first (free); doctor makes live calls and uses quota.\n\
\n\
web_search modes (quick/deep/verify need engine brave; research runs on any):\n\
- quick (default): cheapest. Discovery, docs, URLs, news.\n\
- deep: Tavily advanced with an answer and page text. Use when the answer needs page content.\n\
- verify: Brave and Tavily in parallel, merged. For important or disputed claims; costs both quotas.\n\
\n\
Queries:\n\
- Write the keywords a matching page would contain (official names, versions, exact error text), not a full question and not the answer you expect.\n\
- Split a multi-part question into one search per part; issue independent searches in parallel.\n\
- Narrow with recency_days or time_range/start_date/end_date, include_domains and exclude_domains instead of writing dates or site: into the query.\n\
- Tavily-only options (topic, search_depth, include_answer, ...) apply only when Tavily serves; warnings say when they were ignored.\n\
\n\
Results:\n\
- Cite sources by source_id (src_001). A provider-written answer is a summary to check against sources, never a source itself.\n\
- When notice is present, a fallback provider served the result: tell the user.\n\
- When truncated=true, recover the text with get_sources or web_fetch.\n\
- Count independent sources, not results. Confirm releases, versions and official decisions on the official domain.";

/// Protocol revisions this server speaks, newest first. `initialize` echoes the
/// client's requested version when it is one of these — so existing stdio
/// clients that still request "2024-11-05" keep getting "2024-11-05" — and
/// otherwise declares [`LATEST_PROTOCOL_VERSION`].
pub(crate) const SUPPORTED_PROTOCOL_VERSIONS: &[&str] =
    &["2025-11-25", "2025-06-18", "2025-03-26", "2024-11-05"];

/// Latest revision we support; declared when the client requests nothing or an
/// unsupported version.
pub(crate) const LATEST_PROTOCOL_VERSION: &str = "2025-11-25";

/// Pick the protocol revision to declare in the `initialize` result: the
/// client's requested revision when supported, else our latest.
pub(crate) fn negotiate_protocol_version(requested: Option<&str>) -> &'static str {
    match requested {
        Some(req) => SUPPORTED_PROTOCOL_VERSIONS
            .iter()
            .find(|version| **version == req)
            .copied()
            .unwrap_or(LATEST_PROTOCOL_VERSION),
        None => LATEST_PROTOCOL_VERSION,
    }
}

pub(crate) async fn handle_message(service: &SearchService, request: Value) -> Option<Value> {
    request.get("id")?;
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    Some(
        handle_request(service, request)
            .await
            .unwrap_or_else(|err| {
                let code = err.code() as i64;
                let message = redact_text(&err.to_string(), &service.secret_values());
                error_response(id, code, message)
            }),
    )
}

async fn handle_request(service: &SearchService, request: Value) -> Result<Value> {
    let id = request.get("id").cloned().unwrap_or(Value::Null);
    let method = request
        .get("method")
        .and_then(Value::as_str)
        .ok_or_else(|| GrokSearchError::InvalidParams("missing method".to_string()))?;

    match method {
        "initialize" => {
            // Negotiate: echo the client's requested revision when we speak it
            // (keeps existing stdio clients that still ask for "2024-11-05"
            // working), otherwise declare our latest supported revision.
            let requested = request
                .get("params")
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str);
            Ok(success_response(
                id,
                json!({
                    "protocolVersion": negotiate_protocol_version(requested),
                    "serverInfo": {
                        "name": "llm-search",
                        "version": env!("CARGO_PKG_VERSION")
                    },
                    "capabilities": {
                        "tools": {}
                    },
                    "instructions": SERVER_INSTRUCTIONS
                }),
            ))
        }
        "ping" => Ok(success_response(id, json!({}))),
        "tools/list" => Ok(success_response(id, tools_list())),
        "tools/call" => {
            let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
            let name = params
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| GrokSearchError::InvalidParams("missing tool name".to_string()))?;
            let args = params
                .get("arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let result = redact_value(
                call_tool(service, name, args).await?,
                &service.secret_values(),
            );
            Ok(success_response(
                id,
                json!({
                    "content": [
                        {
                            "type": "text",
                            "text": result.to_string()
                        }
                    ],
                    "structuredContent": result
                }),
            ))
        }
        _ => Err(GrokSearchError::NotFound(format!(
            "unsupported method: {method}"
        ))),
    }
}

async fn call_tool(service: &SearchService, name: &str, args: Value) -> Result<Value> {
    match name {
        "doctor" => {
            let provider = args.get("provider").and_then(Value::as_str);
            service.doctor_scoped(provider).await
        }
        "provider_status" => Ok(service.provider_status()),
        "web_search" => {
            let is_research = args.get("mode").and_then(Value::as_str) == Some("research");
            let (research, research_id) = if is_research {
                crate::tool_params::parse_research_params(&args)?
            } else {
                Default::default()
            };
            // Resuming a research task needs only its id.
            let query = match args.get("query").and_then(Value::as_str) {
                Some(query) => query,
                None if research_id.is_some() => "",
                None => {
                    return Err(GrokSearchError::InvalidParams(
                        "web_search.query is required".into(),
                    ))
                }
            };
            let recency_days = args
                .get("recency_days")
                .and_then(Value::as_u64)
                .map(|value| value as u32)
                .filter(|value| *value > 0);
            let include_domains = parse_string_array(args.get("include_domains"));
            let ext = crate::tool_params::parse_search_extensions(
                &args,
                recency_days.is_some(),
                !include_domains.is_empty(),
            )?;
            let input = WebSearchInput {
                query: query.to_string(),
                // model/platform are intentionally NOT read from tool input: the
                // calling client's LLM must not choose the Grok model or focus
                // platform (issue #15) — it hallucinates names like `grok-4` that
                // override the operator's configured model. The model is fixed by
                // config (GROK_SEARCH_MODEL) or the per-request X-Grok-Model
                // header; leaving these None routes through the default in
                // build_search_request.
                platform: None,
                model: None,
                extra_sources: args
                    .get("extra_sources")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize),
                recency_days,
                include_domains,
                exclude_domains: parse_string_array(args.get("exclude_domains")),
                include_content: args.get("include_content").and_then(Value::as_bool),
                response_format: args
                    .get("response_format")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                mode: args.get("mode").and_then(Value::as_str).map(str::to_string),
                max_results: args
                    .get("max_results")
                    .and_then(Value::as_u64)
                    .map(|value| value as usize),
                time_range: ext.time_range,
                start_date: ext.start_date,
                end_date: ext.end_date,
                hints: ext.hints,
                tavily: ext.tavily,
                research,
                research_id,
            };
            let output = service
                .web_search(input)
                .await
                .map_err(with_fallback_hint)?;
            Ok(serde_json::to_value(output)
                .map_err(|err| GrokSearchError::Parse(format!("serialize output: {err}")))?)
        }
        "get_sources" => {
            let session_id = args
                .get("session_id")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    GrokSearchError::InvalidParams("get_sources.session_id is required".into())
                })?;
            let offset = args
                .get("offset")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .unwrap_or(0);
            let limit = args
                .get("limit")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .filter(|value| *value > 0);
            let output = service.get_sources(session_id, offset, limit).await?;
            Ok(serde_json::to_value(output)
                .map_err(|err| GrokSearchError::Parse(format!("serialize sources: {err}")))?)
        }
        "web_fetch" => {
            let max_chars = args
                .get("max_chars")
                .and_then(Value::as_u64)
                .map(|value| value as usize)
                .filter(|value| *value > 0);
            let options = parse_fetch_options(&args)?;
            if let Some(urls) = args.get("urls") {
                let urls = parse_http_urls(urls)?;
                let output = service.web_fetch_batch(&urls, max_chars, &options).await?;
                return serde_json::to_value(output)
                    .map_err(|err| GrokSearchError::Parse(format!("serialize fetch: {err}")));
            }
            let url = args.get("url").and_then(Value::as_str).ok_or_else(|| {
                GrokSearchError::InvalidParams("web_fetch.url or web_fetch.urls is required".into())
            })?;
            require_http_url(url)?;
            let output = service.web_fetch_with(url, max_chars, &options).await?;
            Ok(serde_json::to_value(output)
                .map_err(|err| GrokSearchError::Parse(format!("serialize fetch: {err}")))?)
        }
        "web_map" => {
            let url = args
                .get("url")
                .and_then(Value::as_str)
                .ok_or_else(|| GrokSearchError::InvalidParams("web_map.url is required".into()))?;
            // Tavily also accepts a bare domain (`docs.tavily.com`), so the
            // URL is not forced to http(s) here.
            let params = crate::tool_params::parse_site_params(&args, "web_map", false)?;
            let mapped = service.web_map_with(url, &params).await?;
            let mapped_sources: Vec<Value> = mapped
                .sources
                .iter()
                .map(|source| json!({ "url": &source.url, "provider": &source.provider }))
                .collect();
            let mut out = json!({
                "url": url,
                "sources_count": mapped_sources.len(),
                "sources": mapped_sources,
            });
            let object = out.as_object_mut().expect("literal object");
            if let Some(base) = mapped.base_url {
                object.insert("base_url".into(), Value::String(base));
            }
            if let Some(id) = mapped.request_id {
                object.insert("request_id".into(), Value::String(id));
            }
            if let Some(usage) = mapped.usage {
                object.insert("usage".into(), usage);
            }
            Ok(out)
        }
        "web_crawl" => {
            let url = args.get("url").and_then(Value::as_str).ok_or_else(|| {
                GrokSearchError::InvalidParams("web_crawl.url is required".into())
            })?;
            let params = crate::tool_params::parse_site_params(&args, "web_crawl", true)?;
            let output = service.web_crawl(url, &params).await?;
            serde_json::to_value(output)
                .map_err(|err| GrokSearchError::Parse(format!("serialize crawl: {err}")))
        }
        _ => Err(GrokSearchError::NotFound(format!("unknown tool: {name}"))),
    }
}

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "web_search",
                "description": "Search the live web. Use for discovery and current facts when you do not have a URL; if the user gave an exact URL, call web_fetch on it first. The server's engine (LLM_SEARCH_PROVIDER) decides the pipeline: `brave` (default) routes Brave / Tavily by `mode` — quick (default): Brave first, Tavily fallback, for broad discovery, docs, URLs, news; deep: Tavily advanced first with a Tavily answer and inline page text, for research; verify: Brave and Tavily in parallel, merged, for important claims that need cross-checking. `grok` returns a Grok-written answer plus sources (mode is ignored). Tavily-only parameters apply only when Tavily serves (deep, verify, or quick after a Brave failure); otherwise `warnings` lists them as ignored. Every result has `source_log` (cite sources by `source_id`, e.g. src_001). When `notice` is present a fallback provider served the result: say so in your answer. When truncated=true, recover trimmed text via web_fetch or get_sources.",
                "inputSchema": {
                    "type": "object",
                    "required": ["query"],
                    "properties": {
                        "query": { "type": "string", "description": "Search keywords; for mode=research a full task description. Pass an empty string when resuming with research_id." },
                        "mode": {
                            "type": "string",
                            "enum": ["quick", "deep", "verify", "research"],
                            "description": "quick = Brave then Tavily (cheapest); deep = Tavily advanced then Brave, with answer and page text (default orders; the operator may reorder them, provider_status shows the live order); verify = Brave + Tavily in parallel, merged (costs both) — these three need engine `brave`. research = Tavily Research report (any engine): ONLY when the user asks for research; query is a full task description, not keywords."
                        },
                        "research_model": { "type": "string", "enum": ["mini", "pro", "auto"], "description": "mode=research. mini for a narrow question, pro for a broad multi-part topic, auto (default) lets Tavily pick." },
                        "output_schema": { "type": "object", "description": "mode=research. JSON Schema (with `properties`) for a structured report; content is then that JSON." },
                        "citation_format": { "type": "string", "enum": ["numbered", "mla", "apa", "chicago"], "description": "mode=research. Citation style (default numbered)." },
                        "output_length": { "type": "string", "enum": ["short", "standard", "long"], "description": "mode=research. Report length (default standard)." },
                        "research_id": { "type": "string", "description": "mode=research. Resume waiting on a task an earlier call returned as still running; query may be omitted." },
                        "max_results": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": 20,
                            "description": "Results to return (engine `brave` only). Default 5."
                        },
                        "extra_sources": {
                            "type": "integer",
                            "minimum": 0,
                            "description": "Engine `grok` only: supplemental source count, served by the configured source chain (default order: Tavily, then Exa, TinyFish, Firecrawl, Brave — first provider with results wins; LLM_SEARCH_SOURCE_PROVIDERS overrides). If omitted, GROK_SEARCH_EXTRA_SOURCES is used. Use max_results with engine `brave`."
                        },
                        "recency_days": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Restrict results to sources published within the last N days. Honored natively by Brave (freshness), Tavily (start_date), Exa (startPublishedDate), and TinyFish (recency window); providers that cannot honor filters (Firecrawl) are skipped for filtered requests. Also hinted to the Grok prompt. Use only one of recency_days, time_range, start_date/end_date."
                        },
                        "time_range": {
                            "type": "string",
                            "enum": ["day", "week", "month", "year"],
                            "description": "Relative publish window (strict filter): Brave pd/pw/pm/py, Tavily time_range, Exa/TinyFish as a start bound. Tavily keeps undated pages unless filter_by_published_date=true."
                        },
                        "start_date": {
                            "type": "string",
                            "description": "Earliest publish date, YYYY-MM-DD (strict filter). Brave freshness range, Tavily start_date, Exa startPublishedDate, TinyFish start bound."
                        },
                        "end_date": {
                            "type": "string",
                            "description": "Latest publish date, YYYY-MM-DD (strict filter). Brave, Tavily and Exa honor it; TinyFish and Firecrawl are skipped."
                        },
                        "country": {
                            "type": "string",
                            "description": "Boost results from a country: ISO 3166-1 alpha-2 code (TW, US) or English name. Hint: Brave country, Tavily country (general topic only); other providers ignore it."
                        },
                        "language": {
                            "type": "string",
                            "description": "Result language code, forwarded as given: Brave search_lang, Tavily language (boost; filter_by_language=true makes Tavily strict). A value a provider rejects makes it fail over."
                        },
                        "safe_search": {
                            "type": "boolean",
                            "description": "Filter adult content: Brave safesearch strict/off (unset = moderate), Tavily safe_search (not with search_depth fast/ultra-fast)."
                        },
                        "exact_match": {
                            "type": "boolean",
                            "description": "Only results containing the quoted phrase(s) in query (Tavily exact_match; Brave already treats quotes as exact)."
                        },
                        "topic": {
                            "type": "string",
                            "enum": ["general", "news", "finance"],
                            "description": "Tavily only. Search category; default general."
                        },
                        "search_depth": {
                            "type": "string",
                            "enum": ["basic", "advanced", "fast", "ultra-fast"],
                            "description": "Tavily only. Overrides the mode default (quick basic, deep/verify advanced). advanced costs 2 credits, others 1."
                        },
                        "chunks_per_source": { "type": "integer", "minimum": 1, "maximum": 3, "description": "Tavily only. Snippets per result (default 3)." },
                        "include_answer": { "type": ["boolean", "string"], "description": "Tavily only. true/basic or advanced generated answer; overrides the mode default." },
                        "include_raw_content": { "type": ["boolean", "string"], "description": "Tavily only. Page text per result: true/markdown or text; overrides the mode default." },
                        "include_images": { "type": "boolean", "description": "Tavily only. Query images in `images`." },
                        "include_image_descriptions": { "type": "boolean", "description": "Tavily only. Describe each image (with include_images)." },
                        "include_favicon": { "type": "boolean", "description": "Tavily only. favicon per source." },
                        "include_domains_mode": { "type": "string", "enum": ["restrict", "prefer"], "description": "Tavily only. prefer = rank include_domains first without excluding others. Needs include_domains." },
                        "include_published_date": { "type": "boolean", "description": "Tavily only. published_date per source." },
                        "filter_by_published_date": { "type": "boolean", "description": "Tavily only. Drop results outside the window, including undated ones." },
                        "filter_by_language": { "type": "boolean", "description": "Tavily only. Make language a strict filter. Needs language." },
                        "auto_parameters": { "type": "boolean", "description": "Tavily only. Let Tavily pick parameters from the query (may use advanced depth, 2 credits)." },
                        "include_usage": { "type": "boolean", "description": "Tavily only. Credit usage in `usage`." },
                        "include_domains": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Only return results from these domains. Brave (site: operator plus host filter) and Tavily/Exa/TinyFish (native domain parameters) honor it strictly; filter-blind providers are skipped. Grok receives it as a soft preference."
                        },
                        "exclude_domains": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Suppress results from these domains. Brave (-site: operator plus host filter) and Tavily/Exa/TinyFish (native domain parameters) honor it strictly; filter-blind providers are skipped. Grok receives it as a soft instruction."
                        },
                        "include_content": {
                            "type": "boolean",
                            "description": "Fetch page text into sources that lack it (via the resolve_content pipeline). Default true for engine `grok`, false for engine `brave` (deep/verify already carry Tavily page text). Superseded by response_format when both are set."
                        },
                        "response_format": {
                            "type": "string",
                            "enum": ["concise", "detailed"],
                            "description": "concise = synthesized answer + source metadata only (smallest payload); detailed = inline source content, subject to the response budget. Takes precedence over include_content."
                        }
                    }
                }
            },
            {
                "name": "get_sources",
                "description": "Return cached sources from a previous web_search call by session_id. Use to re-examine sources already retrieved without issuing a new search — it reuses the prior session and runs no new search or fetch. Paginate with offset/limit: the response reports total_sources and, when more pages remain, next_offset to pass as the next offset.",
                "inputSchema": {
                    "type": "object",
                    "required": ["session_id"],
                    "properties": {
                        "session_id": { "type": "string" },
                        "offset": {
                            "type": "integer",
                            "minimum": 0,
                            "default": 0,
                            "description": "Index of the first source to return. Use next_offset from the previous page to continue."
                        },
                        "limit": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Max sources in this page. Omit to return all remaining sources (still subject to the response budget)."
                        }
                    }
                }
            },
            {
                "name": "web_fetch",
                "description": "Read exact pages. Use first whenever the user supplies a URL; use after web_search when the answer needs source text. GitHub issue/PR, StackOverflow (StackExchange), arXiv, and Wikipedia URLs are parsed into structured Markdown; other pages go through Tavily Extract, then Exa, TinyFish, Firecrawl (first that returns content). Pass `url` for one page or `urls` (up to 20) for several; with several, all non-specialist URLs go to Tavily in one call and any it cannot read fall back one by one, with Tavily's reason kept in `failed`. Tavily options (query, chunks_per_source, include_images, include_favicon, timeout, include_usage) are ignored by specialists and other providers. Returns content with provenance: provider, content_hash (SHA-256 of the returned content), extracted_at, and source_log. Only http(s) URLs are accepted.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": { "type": "string", "description": "One URL to read." },
                        "urls": {
                            "type": "array",
                            "items": { "type": "string" },
                            "minItems": 1,
                            "maxItems": 20,
                            "description": "Several URLs to read; failures are listed in `failed` instead of failing the call. Takes precedence over `url`."
                        },
                        "query": { "type": "string", "description": "Tavily only. Return the passages of each page most relevant to this intent instead of the whole page. Leave unset for tables, lists and data files." },
                        "chunks_per_source": { "type": "integer", "minimum": 1, "maximum": 5, "description": "Tavily only, with query. Passages per page (default 3)." },
                        "include_images": { "type": "boolean", "description": "Tavily only. Image URLs per page in `images`." },
                        "include_favicon": { "type": "boolean", "description": "Tavily only. `favicon` per page." },
                        "timeout": { "type": "number", "minimum": 1, "maximum": 60, "description": "Tavily only. Seconds Tavily may spend per extraction (default 10 basic / 30 advanced)." },
                        "include_usage": { "type": "boolean", "description": "Tavily only. Credit usage in `usage`." },
                        "depth": {
                            "type": "string",
                            "enum": ["basic", "advanced"],
                            "description": "Tavily Extract depth. advanced handles harder pages (tables, embedded content) at twice the credit cost. Omit for Tavily's default."
                        },
                        "format": {
                            "type": "string",
                            "enum": ["markdown", "text"],
                            "description": "Tavily Extract output format. Default markdown."
                        },
                        "max_chars": {
                            "type": "integer",
                            "minimum": 1,
                            "description": "Character cap. With `url`: per page, default LLM_SEARCH_FETCH_MAX_CHARS (unlimited if unset). With `urls`: one budget shared by all pages in order, default LLM_SEARCH_RESPONSE_MAX_CHARS."
                        }
                    }
                }
            },
            {
                "name": "web_map",
                "description": "List a site's URLs through Tavily Map, starting from `url` and following links like a tree. Use when you know the site but not the page, then web_fetch the URLs you need. Steer it with `instructions` (natural language; doubles the credit cost) or regex `select_paths` / `select_domains` / `exclude_paths` / `exclude_domains`. Unset options keep Tavily's defaults (depth 1, breadth 20, limit 50). Cost: 1 credit per 10 URLs (2 with instructions).",
                "inputSchema": {
                    "type": "object",
                    "required": ["url"],
                    "properties": {
                        "url": { "type": "string", "description": "Root URL to map from." },
                        "instructions": { "type": "string", "description": "What pages to return, e.g. \"pages about the Python SDK\"." },
                        "max_depth": { "type": "integer", "minimum": 1, "maximum": 5, "description": "How many link levels from url (default 1)." },
                        "max_breadth": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Links followed per page (default 20)." },
                        "limit": { "type": "integer", "minimum": 1, "description": "Total links processed before stopping (default 50)." },
                        "max_results": { "type": "integer", "minimum": 1, "description": "Alias of limit (kept for compatibility)." },
                        "select_paths": { "type": "array", "items": { "type": "string" }, "description": "Regexes; keep only URL paths matching one (e.g. /docs/.*)." },
                        "select_domains": { "type": "array", "items": { "type": "string" }, "description": "Regexes; keep only these (sub)domains (e.g. ^docs\\.example\\.com$)." },
                        "exclude_paths": { "type": "array", "items": { "type": "string" }, "description": "Regexes; drop URL paths matching one." },
                        "exclude_domains": { "type": "array", "items": { "type": "string" }, "description": "Regexes; drop these (sub)domains." },
                        "allow_external": { "type": "boolean", "description": "Include links to other domains (default true)." },
                        "timeout": { "type": "number", "minimum": 10, "maximum": 150, "description": "Seconds Tavily may spend (default 150)." },
                        "include_usage": { "type": "boolean", "description": "Credit usage in `usage`." }
                    }
                }
            },
            {
                "name": "web_crawl",
                "description": "Read many pages of one site through Tavily Crawl: starts at `url`, follows links like web_map, and extracts each page. Use for a whole docs section or site area; for a few known URLs use web_fetch, for URLs only use web_map. Local server: pages are written to one Markdown file and the result gives `file_path` plus an index with each page's `line`, so read only the parts you need with your file tool. Remote (HTTP) server: pages come back in `sources`, trimmed to the response budget; page the rest with get_sources(session_id). Steer with `instructions` or regex path/domain filters; keep `limit` small. Cost: map cost plus 1 credit per 5 pages (2 with extract_depth advanced). Needs a Tavily key.",
                "inputSchema": {
                    "type": "object",
                    "required": ["url"],
                    "properties": {
                        "url": { "type": "string", "description": "Root URL to crawl from." },
                        "instructions": { "type": "string", "description": "Which pages to return, in natural language (doubles the mapping cost)." },
                        "chunks_per_source": { "type": "integer", "minimum": 1, "maximum": 5, "description": "With instructions: relevant passages per page instead of the whole page (default 3)." },
                        "max_depth": { "type": "integer", "minimum": 1, "maximum": 5, "description": "Link levels from url (default 1)." },
                        "max_breadth": { "type": "integer", "minimum": 1, "maximum": 500, "description": "Links followed per page (default 20)." },
                        "limit": { "type": "integer", "minimum": 1, "description": "Total links processed before stopping (default 50)." },
                        "select_paths": { "type": "array", "items": { "type": "string" }, "description": "Regexes; crawl only URL paths matching one (e.g. /docs/.*)." },
                        "select_domains": { "type": "array", "items": { "type": "string" }, "description": "Regexes; crawl only these (sub)domains." },
                        "exclude_paths": { "type": "array", "items": { "type": "string" }, "description": "Regexes; skip URL paths matching one." },
                        "exclude_domains": { "type": "array", "items": { "type": "string" }, "description": "Regexes; skip these (sub)domains." },
                        "allow_external": { "type": "boolean", "description": "Include links to other domains (default true)." },
                        "include_images": { "type": "boolean", "description": "Image URLs per page." },
                        "extract_depth": { "type": "string", "enum": ["basic", "advanced"], "description": "advanced handles tables and embedded content at twice the extraction cost." },
                        "format": { "type": "string", "enum": ["markdown", "text"], "description": "Page text format (default markdown)." },
                        "include_favicon": { "type": "boolean", "description": "favicon per page." },
                        "timeout": { "type": "number", "minimum": 10, "maximum": 150, "description": "Seconds Tavily may spend (default 150)." },
                        "include_usage": { "type": "boolean", "description": "Credit usage in `usage`." }
                    }
                }
            },
            {
                "name": "provider_status",
                "description": "Configuration check that makes no provider request and costs no quota: which providers (Brave, Tavily, Exa, TinyFish, Firecrawl, Grok) are configured, the active web_search engine, and the order each tool tries providers in. Never returns secrets. Use this first when checking setup.",
                "inputSchema": { "type": "object", "properties": {} }
            },
            {
                "name": "doctor",
                "description": "Live health check: calls each provider (Brave, Tavily, Exa, TinyFish, Firecrawl, Grok) and reports status ok / not_configured / auth_error / rate_limited / timeout / error, plus masked configuration. It may consume provider quota, so call it only for explicit diagnostics; use provider_status for a free configuration check.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "provider": {
                            "type": "string",
                            "enum": ["all", "brave", "tavily", "exa", "tinyfish", "firecrawl", "grok"],
                            "description": "Check one provider only. Default all."
                        }
                    }
                }
            }
        ]
    })
}

/// Append [`NATIVE_SEARCH_FALLBACK_HINT`] to upstream failures. Parameter and
/// configuration errors keep their own message: the caller can fix those.
fn with_fallback_hint(err: GrokSearchError) -> GrokSearchError {
    match err {
        GrokSearchError::Provider(message) => {
            GrokSearchError::Provider(format!("{message}\n\n{NATIVE_SEARCH_FALLBACK_HINT}"))
        }
        GrokSearchError::Timeout(message) => {
            GrokSearchError::Timeout(format!("{message}\n\n{NATIVE_SEARCH_FALLBACK_HINT}"))
        }
        other => other,
    }
}

/// `depth` (`basic` / `advanced`) and `format` (`markdown` / `text`).
fn parse_fetch_options(args: &Value) -> Result<FetchOptions> {
    let depth = match args.get("depth").and_then(Value::as_str) {
        None => None,
        Some("basic") => Some(SearchDepth::Basic),
        Some("advanced") => Some(SearchDepth::Advanced),
        Some(other) => {
            return Err(GrokSearchError::InvalidParams(format!(
                "web_fetch.depth must be \"basic\" or \"advanced\", got \"{other}\""
            )))
        }
    };
    let format = match args.get("format").and_then(Value::as_str) {
        None | Some("markdown") => ExtractFormat::Markdown,
        Some("text") => ExtractFormat::Text,
        Some(other) => {
            return Err(GrokSearchError::InvalidParams(format!(
                "web_fetch.format must be \"markdown\" or \"text\", got \"{other}\""
            )))
        }
    };
    Ok(FetchOptions {
        depth,
        format,
        tavily: crate::tool_params::parse_extract_extras(args)?,
    })
}

/// Only HTTP(S) URLs are fetched.
fn require_http_url(url: &str) -> Result<()> {
    match url::Url::parse(url.trim()) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => Ok(()),
        _ => Err(GrokSearchError::InvalidParams(format!(
            "web_fetch only accepts http(s) URLs, got \"{url}\""
        ))),
    }
}

/// `urls`: 1 to [`MAX_FETCH_URLS`] HTTP(S) URLs.
fn parse_http_urls(value: &Value) -> Result<Vec<String>> {
    let urls = parse_string_array(Some(value));
    if urls.is_empty() || urls.len() > MAX_FETCH_URLS {
        return Err(GrokSearchError::InvalidParams(format!(
            "web_fetch.urls must hold 1 to {MAX_FETCH_URLS} URLs"
        )));
    }
    for url in &urls {
        require_http_url(url)?;
    }
    Ok(urls)
}

fn parse_string_array(value: Option<&Value>) -> Vec<String> {
    value
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.as_str())
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn success_response(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result
    })
}

pub(crate) fn error_response(id: Value, code: i64, message: String) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::search::SearchFilters;
    use crate::model::source::{FetchedPage, Source};
    use crate::service::SourceProvider;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    #[tokio::test]
    async fn initialized_notification_does_not_emit_response() {
        let service = SearchService::fake_with_sources();
        let response = handle_message(
            &service,
            json!({
                "jsonrpc": "2.0",
                "method": "notifications/initialized",
                "params": {}
            }),
        )
        .await;

        assert_eq!(response, None);
    }

    #[test]
    fn negotiate_protocol_version_prefers_supported_client_version() {
        // Backward compatibility: an old client asking for 2024-11-05 is echoed.
        assert_eq!(negotiate_protocol_version(Some("2024-11-05")), "2024-11-05");
        assert_eq!(negotiate_protocol_version(Some("2025-06-18")), "2025-06-18");
        assert_eq!(negotiate_protocol_version(Some("2025-11-25")), "2025-11-25");
        // Unknown / absent -> our latest.
        assert_eq!(
            negotiate_protocol_version(Some("1999-01-01")),
            LATEST_PROTOCOL_VERSION
        );
        assert_eq!(negotiate_protocol_version(None), LATEST_PROTOCOL_VERSION);
    }

    #[tokio::test]
    async fn initialize_echoes_legacy_client_version() {
        let service = SearchService::fake_with_sources();
        let response = handle_request(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "initialize",
                "params": { "protocolVersion": "2024-11-05", "capabilities": {} }
            }),
        )
        .await
        .expect("initialize response");
        // An existing stdio client must still see its own requested revision.
        assert_eq!(response["result"]["protocolVersion"], "2024-11-05");
        assert_eq!(response["result"]["serverInfo"]["name"], "llm-search");
    }

    #[tokio::test]
    async fn initialize_returns_server_instructions() {
        let service = SearchService::fake_with_sources();
        let response = handle_request(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 3,
                "method": "initialize",
                "params": { "protocolVersion": "2025-11-25", "capabilities": {} }
            }),
        )
        .await
        .expect("initialize response");
        assert_eq!(response["result"]["instructions"], SERVER_INSTRUCTIONS);
    }

    #[test]
    fn instructions_fit_client_budget_and_name_every_tool() {
        // Claude Code cuts server instructions at 4,096 UTF-16 code units
        // (2,048 in older releases); anything past that is silently lost.
        let units = SERVER_INSTRUCTIONS.encode_utf16().count();
        assert!(units <= 2048, "instructions are {units} UTF-16 units");
        assert!(SERVER_INSTRUCTIONS.is_ascii());
        // A renamed or added tool must not leave the guide pointing at a
        // tool that no longer exists, or silent about a new one.
        let tools = tools_list();
        for tool in tools["tools"].as_array().expect("tools array") {
            let name = tool["name"].as_str().expect("tool name");
            assert!(
                SERVER_INSTRUCTIONS.contains(name),
                "instructions do not mention tool {name}"
            );
        }
    }

    #[tokio::test]
    async fn initialize_declares_latest_for_unknown_version() {
        let service = SearchService::fake_with_sources();
        let response = handle_request(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 2,
                "method": "initialize",
                "params": { "protocolVersion": "3000-01-01", "capabilities": {} }
            }),
        )
        .await
        .expect("initialize response");
        assert_eq!(
            response["result"]["protocolVersion"],
            LATEST_PROTOCOL_VERSION
        );
    }

    #[tokio::test]
    async fn ping_request_gets_empty_success_response() {
        let service = SearchService::fake_with_sources();
        let response = handle_request(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "ping"
            }),
        )
        .await
        .expect("ping response");

        assert_eq!(response["id"], 7);
        assert_eq!(response["result"], json!({}));
    }

    #[tokio::test]
    async fn web_map_returns_url_sources_without_search_metadata() {
        let service = SearchService::fake_with_sources();
        let response = handle_request(
            &service,
            json!({
                "jsonrpc": "2.0",
                "id": 9,
                "method": "tools/call",
                "params": {
                    "name": "web_map",
                    "arguments": {
                        "url": "https://example.com",
                        "max_results": 2
                    }
                }
            }),
        )
        .await
        .expect("web_map response");

        let output = &response["result"]["structuredContent"];
        let sources = output["sources"].as_array().expect("sources");
        assert_eq!(output["sources_count"], 2);
        assert_eq!(
            sources[0],
            json!({
                "url": "https://example.com/page-0",
                "provider": "tavily"
            })
        );
        assert!(sources[0].get("title").is_none());
        assert!(sources[0].get("description").is_none());
        assert!(sources[0].get("published_date").is_none());
    }

    #[test]
    fn tools_list_descriptions_guide_routing() {
        let listed = tools_list();
        let tools = listed["tools"].as_array().expect("tools array");

        let desc = |name: &str| -> String {
            tools
                .iter()
                .find(|t| t["name"] == name)
                .unwrap_or_else(|| panic!("tool {name} missing"))["description"]
                .as_str()
                .unwrap_or_else(|| panic!("tool {name} description not a string"))
                .to_string()
        };

        // web_search: discovery cue and the no-URL case; routes exact URLs
        // to web_fetch; names the three router modes; asks for citation by
        // source_log id and disclosure of a fallback notice. It must NOT
        // claim the single-page-read role (that's web_fetch's job).
        let web_search = desc("web_search");
        assert!(web_search.contains("discovery"), "web_search: {web_search}");
        assert!(
            web_search.contains("do not have a URL"),
            "web_search: {web_search}"
        );
        assert!(web_search.contains("web_fetch"), "web_search: {web_search}");
        for mode in ["quick", "deep", "verify"] {
            assert!(
                web_search.contains(mode),
                "web_search lacks {mode}: {web_search}"
            );
        }
        assert!(
            web_search.contains("source_log"),
            "web_search: {web_search}"
        );
        assert!(web_search.contains("notice"), "web_search: {web_search}");
        assert!(
            !web_search.contains("read a single page"),
            "web_search must not claim the single-page-read role: {web_search}"
        );

        // web_fetch: exact-URL reads first, names all four special sources,
        // reports provenance, cross-references web_search.
        let web_fetch = desc("web_fetch");
        assert!(web_fetch.contains("exact pages"), "web_fetch: {web_fetch}");
        assert!(
            web_fetch.contains("user supplies a URL"),
            "web_fetch: {web_fetch}"
        );
        assert!(web_fetch.contains("GitHub issue"), "web_fetch: {web_fetch}");
        assert!(
            web_fetch.contains("StackOverflow") || web_fetch.contains("StackExchange"),
            "web_fetch: {web_fetch}"
        );
        assert!(web_fetch.contains("arXiv"), "web_fetch: {web_fetch}");
        assert!(web_fetch.contains("Wikipedia"), "web_fetch: {web_fetch}");
        assert!(web_fetch.contains("content_hash"), "web_fetch: {web_fetch}");
        assert!(web_fetch.contains("web_search"), "web_fetch: {web_fetch}");

        // provider_status is free; doctor warns that it costs quota.
        let status = desc("provider_status");
        assert!(
            status.contains("no provider request"),
            "provider_status: {status}"
        );
        let doctor = desc("doctor");
        assert!(doctor.contains("may consume"), "doctor: {doctor}");

        // get_sources: reuses a prior web_search session, runs no new search.
        let get_sources = desc("get_sources");
        assert!(
            get_sources.contains("session_id"),
            "get_sources: {get_sources}"
        );
        assert!(
            get_sources.contains("new search"),
            "get_sources: {get_sources}"
        );
    }

    #[test]
    fn web_search_schema_hides_model_and_platform() {
        // issue #15: the calling client's LLM must not be offered `model` or
        // `platform` — it fills them with hallucinated values (e.g. `grok-4`)
        // that override the operator's configured model. The schema must not
        // advertise them, so the client never learns they exist.
        let listed = tools_list();
        let tools = listed["tools"].as_array().expect("tools array");
        let web_search = tools
            .iter()
            .find(|t| t["name"] == "web_search")
            .expect("web_search tool present");
        let props = web_search["inputSchema"]["properties"]
            .as_object()
            .expect("web_search inputSchema.properties object");

        assert!(
            !props.contains_key("model"),
            "web_search must not expose `model`: {props:?}"
        );
        assert!(
            !props.contains_key("platform"),
            "web_search must not expose `platform`: {props:?}"
        );
        // Guard against an over-broad deletion: the real parameters stay.
        assert!(props.contains_key("query"), "query must remain: {props:?}");
        assert!(
            props.contains_key("response_format"),
            "response_format must remain: {props:?}"
        );
    }

    /// Drive the serve loop over in-memory buffers instead of process stdio,
    /// returning one parsed value per response line. Every characterization
    /// test below observes the loop only through what a client would see:
    /// bytes in, response lines out.
    async fn drive(input: &str) -> Vec<Value> {
        drive_with(SearchService::fake_with_sources(), input).await
    }

    /// Same, with a caller-supplied service so a test can inject a provider
    /// that reports what the loop is doing while it runs.
    async fn drive_with(service: SearchService, input: &str) -> Vec<Value> {
        let mut output: Vec<u8> = Vec::new();
        serve(service, input.as_bytes(), &mut output)
            .await
            .expect("serve loop ran to end of input");
        String::from_utf8(output)
            .expect("responses are utf-8")
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("each response line is json"))
            .collect()
    }

    /// A source provider that sleeps for the number of milliseconds named by
    /// the query, recording how many calls are in flight at once. Asserting on
    /// the peak says whether requests actually overlapped, without depending
    /// on wall-clock ordering the way a duration comparison would.
    #[derive(Clone, Default)]
    struct ConcurrencyProbe {
        in_flight: Arc<AtomicUsize>,
        peak: Arc<AtomicUsize>,
    }

    impl ConcurrencyProbe {
        fn peak(&self) -> usize {
            self.peak.load(Ordering::SeqCst)
        }

        fn provider(&self) -> Arc<dyn SourceProvider> {
            Arc::new(self.clone())
        }
    }

    #[async_trait::async_trait]
    impl SourceProvider for ConcurrencyProbe {
        async fn search_sources(
            &self,
            query: &str,
            _max_results: usize,
            _filters: &SearchFilters,
        ) -> Result<Vec<Source>> {
            let now = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.peak.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(query.parse().unwrap_or(0))).await;
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            Ok(Vec::new())
        }

        async fn fetch(&self, url: &str) -> Result<FetchedPage> {
            Err(GrokSearchError::Provider(format!("no fetch for {url}")))
        }

        async fn map(&self, _url: &str, _max_results: usize) -> Result<Vec<Source>> {
            Ok(Vec::new())
        }
    }

    /// A `web_search` call whose query is the number of milliseconds the probe
    /// should spend on it. `extra_sources: 0` keeps enrichment out of the way
    /// so the delay is the only thing the test is timing.
    fn probe_request(id: u64, delay_millis: u64) -> String {
        json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": "tools/call",
            "params": {
                "name": "web_search",
                "arguments": { "query": delay_millis.to_string(), "extra_sources": 0 }
            }
        })
        .to_string()
    }

    #[tokio::test]
    async fn serve_handles_requests_concurrently() {
        let probe = ConcurrencyProbe::default();
        let service = SearchService::fake_custom(None, probe.provider(), None, [("", "")]);

        let responses = drive_with(
            service,
            &format!("{}\n{}\n", probe_request(1, 150), probe_request(2, 150)),
        )
        .await;

        assert_eq!(responses.len(), 2, "{responses:?}");
        assert!(
            probe.peak() >= 2,
            "requests were handled one at a time; peak in flight was {}",
            probe.peak()
        );
    }

    #[tokio::test]
    async fn serve_answers_in_completion_order_with_ids_intact() {
        let probe = ConcurrencyProbe::default();
        let service = SearchService::fake_custom(None, probe.provider(), None, [("", "")]);

        // The slow request arrives first. A client that follows it with a quick
        // one must not wait out the slow one to hear back.
        let responses = drive_with(
            service,
            &format!("{}\n{}\n", probe_request(1, 250), probe_request(2, 0)),
        )
        .await;

        assert_eq!(responses.len(), 2, "{responses:?}");
        assert_eq!(
            responses[0]["id"], 2,
            "the quick request queued behind the slow one: {responses:?}"
        );
        assert_eq!(responses[1]["id"], 1);
    }

    #[tokio::test]
    async fn serve_caps_in_flight_requests_without_dropping_any() {
        let probe = ConcurrencyProbe::default();
        let service = SearchService::fake_custom(None, probe.provider(), None, [("", "")]);

        let burst: String = (1..=12)
            .map(|id| format!("{}\n", probe_request(id, 80)))
            .collect();
        let responses = drive_with(service, &burst).await;

        // Answered, all of them: over the cap means wait, never refuse or drop.
        let mut ids: Vec<u64> = responses
            .iter()
            .map(|response| {
                response["id"]
                    .as_u64()
                    .expect("every response carries an id")
            })
            .collect();
        ids.sort_unstable();
        assert_eq!(ids, (1..=12).collect::<Vec<u64>>(), "{responses:?}");

        assert!(
            probe.peak() <= MAX_IN_FLIGHT,
            "cap of {MAX_IN_FLIGHT} exceeded: peak in flight was {}",
            probe.peak()
        );
        assert!(
            probe.peak() > 1,
            "a burst this size should overlap: peak in flight was {}",
            probe.peak()
        );
    }

    #[tokio::test]
    async fn serve_negotiates_protocol_version_over_in_memory_io() {
        let responses = drive(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{}}}
"#,
        )
        .await;

        assert_eq!(
            responses.len(),
            1,
            "one request, one response: {responses:?}"
        );
        assert_eq!(responses[0]["id"], 1);
        assert_eq!(responses[0]["result"]["protocolVersion"], "2024-11-05");
    }

    #[tokio::test]
    async fn serve_declares_latest_protocol_version_for_an_unknown_request() {
        let responses = drive(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"1999-01-01","capabilities":{}}}
"#,
        )
        .await;

        assert_eq!(
            responses[0]["result"]["protocolVersion"],
            LATEST_PROTOCOL_VERSION
        );
    }

    #[tokio::test]
    async fn serve_emits_no_response_for_a_notification() {
        let responses = drive(
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}
"#,
        )
        .await;

        assert!(
            responses.is_empty(),
            "a message without an id is a notification: {responses:?}"
        );
    }

    #[tokio::test]
    async fn serve_reports_a_parse_error_and_keeps_serving() {
        // The second line proves the loop survives the first: a client that
        // sends one bad frame must not lose the rest of its session.
        let responses = drive(
            r#"not json at all
{"jsonrpc":"2.0","id":7,"method":"ping"}
"#,
        )
        .await;

        assert_eq!(responses.len(), 2, "{responses:?}");
        assert_eq!(responses[0]["error"]["code"], -32700);
        assert_eq!(responses[0]["id"], Value::Null);
        assert_eq!(responses[1]["id"], 7);
        assert_eq!(responses[1]["result"], json!({}));
    }

    #[tokio::test]
    async fn serve_skips_blank_lines() {
        let responses = drive(
            r#"
{"jsonrpc":"2.0","id":7,"method":"ping"}


"#,
        )
        .await;

        assert_eq!(
            responses.len(),
            1,
            "blank lines are framing, not messages: {responses:?}"
        );
        assert_eq!(responses[0]["id"], 7);
    }
}
