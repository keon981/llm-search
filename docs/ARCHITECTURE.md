# Architecture

llm-search is a Rust MCP server for web search and page reading. One service layer routes each tool call to external search providers or to key-free specialist parsers, and records where every result came from.

```text
MCP client
  -> stdio (default) or Streamable HTTP (src/http.rs, feature `http`)
  -> src/mcp.rs            tool schemas, server instructions, JSON-RPC
  -> src/tool_params.rs    argument validation
  -> src/service.rs        SearchService: routing, fallback, budgets
       web_search
         engine brave (default): mode router over providers
           quick / deep  -> ordered chain (LLM_SEARCH_QUICK_ORDER / _DEEP_ORDER)
           verify        -> Brave + Tavily in parallel, merged
           research      -> Tavily Research (also on engine grok)
         engine grok: Grok answer + source chain for extra / fallback sources
           protocol responses        -> src/providers/grok.rs
           protocol chat_completions -> src/providers/openai_compatible.rs
       web_fetch  -> specialists (src/sources/) -> provider chain
       web_map / web_crawl -> Tavily
  -> src/providers/        brave, tavily, exa, tinyfish, firecrawl, grok, openai_compatible
  -> src/cache.rs          session cache behind get_sources
```

## Tools

| Tool | Served by |
|---|---|
| `web_search` | The engine chosen by `LLM_SEARCH_PROVIDER` (`brave` or `grok`); `mode=research` always uses Tavily Research. |
| `get_sources` | The session cache, by `session_id` (search results and remote crawl pages). No provider call. |
| `web_fetch` | Specialist parsers first, then the provider chain. |
| `web_map` | Tavily Map. |
| `web_crawl` | Tavily Crawl. |
| `provider_status` | Configuration only: configured providers, active engine, per-mode order. No provider call. |
| `doctor` | Live probe of each configured provider, classified as `ok`, `not_configured`, `auth_error`, `rate_limited`, `timeout`, or `error`. |

## Engine `brave` (default)

`web_search` picks providers by `mode`:

- **quick** — default order Brave, Tavily, then the rest of the source chain. The first provider with results serves the response.
- **deep** — default order Tavily (advanced depth, with an answer and page text), Brave, then the rest of the chain.
- **verify** — Brave and Tavily in parallel, merged and deduplicated (`search_provider: "mixed"` when both answered); the rest of the chain only if both fail.
- **research** — starts (or, with `research_id`, resumes) a Tavily Research task and polls it (2 s growing ×1.5 to 10 s) up to the wait limit; past the limit it returns the task id and status instead of failing.

`LLM_SEARCH_QUICK_ORDER` / `LLM_SEARCH_DEEP_ORDER` reorder quick and deep: listed providers first, unlisted configured ones behind them in the default order. Parameters never change the order.

Request parameters come in three kinds:

| Kind | Examples | Provider that cannot honor it |
|---|---|---|
| Strict filter | date window, `include_domains` / `exclude_domains` | Skipped for this request |
| Hint | `country`, `language`, `safe_search`, `exact_match` | Ignores it; `warnings` says so |
| Tavily-only | `topic`, `search_depth`, `include_answer`, … | Applied only when Tavily serves; otherwise listed in `warnings` |

## Engine `grok`

The Grok engine asks a model with built-in web search to write the answer, then adds sources from the source chain.

- `GROK_SEARCH_PROTOCOL` selects the API format at `GROK_SEARCH_URL`: `responses` (`/v1/responses` with the `web_search` tool and optional `x_search`) or `chat_completions` (`/v1/chat/completions`). Key and model are the same settings either way.
- Authentication is separate from the protocol: `api_key` mode sends `GROK_SEARCH_API_KEY`; `oauth` mode reads the local token file, refreshes it near expiry, and always uses `responses`. `llm-search login` listens once on `127.0.0.1:56121` for the browser callback, stores the token, and exits.
- After a verifiable answer, `GROK_SEARCH_EXTRA_SOURCES` supplemental sources come from the first chain provider with results.
- `web_search` falls back to the source chain when the Grok request fails, the answer is empty, or it has no verifiable sources. The response exposes `search_provider`, `fallback_used`, and `fallback_reason`.

## Source chain

The ordered list of configured providers: `tavily, exa, tinyfish, firecrawl, brave` by default, or exactly `LLM_SEARCH_SOURCE_PROVIDERS` when set. It serves search fallback in both engines and generic `web_fetch`. The first provider with usable output wins. Providers that cannot honor a strict filter are skipped instead of silently violating it. One deadline covers the whole walk, so a hanging provider stops the walk instead of giving every later provider a fresh full timeout.

`web_map` and `web_crawl` are not chain positions: they use Tavily whenever `TAVILY_API_KEY` is configured, even when the chain leaves Tavily out.

## web_fetch pipeline

1. **Specialists** (GitHub issues / PRs / releases, StackExchange, arXiv, Wikipedia) parse their URLs into structured Markdown with no API key.
2. **Batch**: with several `urls`, every URL the specialists did not serve goes to the first fetch-capable provider in one call when it has a batch endpoint (Tavily Extract, up to 20 URLs). URLs that come back empty are retried once at `advanced` depth.
3. **Per URL**: anything still unread continues down the rest of the chain one URL at a time, keeping Tavily's failure reason.

Results, `failed`, `source_log` ids, and the shared character budget stay in input order. Brave is search-only and never fetches.

## web_crawl output

The stdio server writes all crawled pages to one Markdown file under `LLM_SEARCH_OUTPUT_DIR` (owner-only permissions) and returns an index with each page's starting line, so the agent can read only the parts it needs. The HTTP transport never writes to its own disk: pages come back inline within the response budget and the rest is reachable through `get_sources`.

## Provenance

- Every result carries a `source_log` entry (`src_001`, …). Fetched pages also carry a SHA-256 `content_hash` and `extracted_at`.
- Engine `brave`: `search_provider` names the provider that served the result (`mixed` for a merged verify result); `fallbacks` lists the providers that failed and why, and `notice` asks the agent to disclose a fallback.
- Engine `grok`: each source's `provider` field is `grok_responses` / `grok_chat_completions` for the model's own citations, `{provider}_enrichment` for supplemental sources, and `{provider}_fallback` for sources used because the Grok answer failed or was unverifiable.
- Every API key value is redacted from tool output and error messages.

## Transports

**stdio** (default): JSON-RPC over stdin / stdout with `initialize`, `tools/list`, and `tools/call`. `initialize` returns `instructions` — the server's usage guide (which tool to pick, `web_search` modes, query and citation rules), kept within 2,048 UTF-16 code units; a unit test enforces the cap and that every tool is named. Tool results are JSON inside MCP text content.

**Streamable HTTP** (`--http`, `serve`, or `LLM_SEARCH_BIND`; build with `--features http`): a stateless, multi-tenant server on `/mcp`.

- The server stores no credentials. Each request builds its own config from the operator's non-secret environment plus the caller's headers (`X-Brave-Api-Key`, `X-Grok-Base-Url`, …).
- API keys, `GROK_SEARCH_PROTOCOL`, and OAuth settings are stripped from the operator environment, so the remote server speaks only the Responses protocol.
- A caller-supplied Grok gateway must be HTTPS and is called through a DNS-pinned client that refuses redirects.
- Cached sessions are namespaced per caller key, so one tenant cannot read another's `get_sources` pages.
- `LLM_SEARCH_ALLOWED_ORIGINS` optionally restricts browser origins.
