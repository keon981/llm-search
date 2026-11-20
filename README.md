# llm-search

English | [繁體中文](README.zh.md)

A fast web search and page-fetching MCP server for AI coding agents (Claude Code, Codex, and others), written in Rust.

- **Smart search routing**: Routes queries across Brave and Tavily depending on the mode—fast lookups, deep content extraction, cross-checked results, or full research reports—with automatic fallback to Exa, TinyFish, and Firecrawl. Prefer Grok's synthesized answers? You can switch engines with one env var.
- **Zero-key parsers + fallback fetch**: Reads GitHub (issues, PRs, releases), StackExchange, arXiv, and Wikipedia natively with no API key required. Any other URL flows through Tavily → Exa → TinyFish → Firecrawl until one succeeds.
- **Traceable and safe by default**: Every result gets a `source_log` ID (`src_001`, etc.), fetched pages include a SHA-256 `content_hash` and `extracted_at` timestamp, fallback providers are always reported (`fallbacks` / `notice`), and API keys are redacted from all responses and error messages.

Forked from [GrokSearch-rs](https://github.com/Episkey-G/GrokSearch-rs), with routing and traceability ideas adapted from [deepseek-harness-web-search-plugin](https://github.com/cloveric/deepseek-harness-web-search-plugin). See [Credits](#credits).

---

## Quick Start

### 1. Install

The npm package ships prebuilt binaries for macOS (universal), Linux (x64 / arm64), and Windows (x64 / arm64). No Rust toolchain needed.

```bash
npm install -g @keon981/llm-search
# or with Vite+:
vp install -g @keon981/llm-search

llm-search --version
```

To update later, just run the install command again.

### 2. Connect to Your Agent

**Claude Code**

```bash
claude mcp add llm-search -s user \
  -e BRAVE_API_KEY=YOUR_BRAVE_KEY \
  -e TAVILY_API_KEY=YOUR_TAVILY_KEY \
  -- llm-search
```

This writes the config to `mcpServers.llm-search.env` in `~/.claude.json`, where you can swap in your real keys at any time. Start a new session and ask Claude to run `provider_status` to verify everything is wired up.

**Codex (`config.toml`)**

```toml
[mcp_servers.llm-search]
type = "stdio"
command = "llm-search"

[mcp_servers.llm-search.env]
BRAVE_API_KEY = ""
TAVILY_API_KEY = ""
```

> You only need **one** of `BRAVE_API_KEY`, `TAVILY_API_KEY`, `EXA_API_KEY`, `TINYFISH_API_KEY`, or `FIRECRAWL_API_KEY` to get started with `web_search`. Adding both Brave and Tavily unlocks the full routing modes, and Tavily is required for `web_map`, `web_crawl`, and `mode=research`.

---

## Tools

| Tool | What it does |
|---|---|
| `web_search` | Search the web with filters for result count (`max_results`, 1–20, default 5), date range, domains, country/language, safe search, and exact match. Supports `quick`, `deep`, `verify`, and `research` modes. |
| `get_sources` | Page through cached sources from your last `web_search` or remote `web_crawl` (`offset` / `limit`) without triggering a new API call. |
| `web_fetch` | Read full page content from a single `url` or up to 20 `urls` in one call. Supports `markdown` / `text` output, `basic` / `advanced` depth, character limits (`max_chars`), and Tavily extraction options. |
| `web_map` | Discover URLs on a site via Tavily Map, with depth/breadth limits, custom instructions, and regex filters for paths and domains. |
| `web_crawl` | Crawl linked pages across a site via Tavily Crawl. Locally, saves everything to a single Markdown file and returns each page's line range; in remote HTTP mode, returns content inline with `get_sources` pagination. |
| `provider_status` | Offline config check—makes no API calls and uses zero quota. Shows configured providers, active engine, and fallback order per tool. |
| `doctor` | Live connectivity check against each configured provider (uses a small amount of quota). Reports `ok`, `not_configured`, `auth_error`, `rate_limited`, `timeout`, or `error`. Pass `provider` to test just one. |

### Search Modes (`web_search`)

When using the default `brave` engine, pick a `mode` based on how thorough you need the search to be:

| Mode | How it runs | Best for |
|---|---|---|
| `quick` *(default)* | Brave first → falls back to Tavily on failure | Everyday lookups, docs, and news |
| `deep` | Tavily `advanced` (includes a short answer + up to 6,000 chars per page) → falls back to Brave | Research when you need full page context, not just snippets |
| `verify` | Brave and Tavily in parallel, merged and deduplicated | Fact-checking or high-stakes claims *(uses quota on both)* |
| `research` | Tavily Research: multi-source report with citations *(works with any engine; requires Tavily key)* | Dedicated research reports when explicitly requested |

If both Brave and Tavily fail, search falls back through **Exa → TinyFish → Firecrawl**. These are the default `quick` / `deep` orders; change them with `LLM_SEARCH_QUICK_ORDER` / `LLM_SEARCH_DEEP_ORDER` (see [Search Engine](#search-engine)). `provider_status` shows the order in effect.

A few useful details:
- **Date filtering**: Pass one of `recency_days`, `time_range`, or `start_date` / `end_date`. All three apply strict date filters on Brave, Tavily, and Exa. TinyFish only supports a start date, so it is skipped whenever `end_date` is set.
- **Tavily-specific options**: Parameters like `topic`, `search_depth`, `chunks_per_source`, `include_answer`, `include_raw_content`, `include_images`, and `include_domains_mode` only take effect when Tavily actually handles the request (`deep`, `verify`, or a fallback). Otherwise they are ignored and noted in `warnings`.
- **Long-running `research` jobs**: `mode=research` waits up to 300s for `mini` or 900s for `pro` / `auto` (override with `LLM_SEARCH_RESEARCH_MAX_WAIT_SECONDS`). If it is still running at the timeout, it returns a `research_id` and status—call `web_search` again with that `research_id` to keep waiting. Supports `research_model`, `output_schema`, `citation_format`, and `output_length`. Note that `include_domains` acts as a soft preference (up to 20 domains) rather than a strict filter in research mode.

### How `web_fetch` Works

1. **Built-in parsers (no API key needed)**: GitHub issues/PRs/releases, StackExchange, arXiv, and Wikipedia are parsed directly into structured Markdown.
2. **General web pages**: Routed through **Tavily Extract → Exa → TinyFish → Firecrawl**, returning the first non-empty result. If Tavily's default depth comes back empty (common on JS-rendered pages), it automatically retries once with `advanced`.
3. **Batch requests (`urls`)**: When Tavily is first in line, all non-specialized URLs are bundled into a single Tavily call. Any pages Tavily misses fall back one by one to the remaining providers, keeping Tavily's original error reason in `failed`.
4. Brave is search-only and does not participate in `web_fetch`.

---

## Configuration

### Search Engine

| Variable | Default | Description |
|---|---|---|
| `LLM_SEARCH_PROVIDER` | `brave` | Search engine for `web_search`. `brave` enables Brave/Tavily routing; `grok` uses Grok synthesized answers (`mode` is ignored). Unrecognized values log a warning and default to `brave`. |
| `LLM_SEARCH_QUICK_ORDER` | `brave, tavily, exa, tinyfish, firecrawl` | Provider order for `mode=quick`. Accepts `a,b,c` or `[a, b, c]`. Listed providers go first in that order; unlisted ones follow in the default order. |
| `LLM_SEARCH_DEEP_ORDER` | `tavily, brave, exa, tinyfish, firecrawl` | Provider order for `mode=deep`, same rules. |
| `BRAVE_API_KEY` | — | Brave Search API key (`BRAVE_SEARCH_API_KEY` is also accepted). |
| `BRAVE_API_URL` | `https://api.search.brave.com` | Brave API base URL. |
| `BRAVE_ENABLED` | `true` | Set to `false` to disable Brave. |
| `TAVILY_API_KEY` | — | Tavily API key. Accepts comma-separated keys for round-robin rotation, automatically switching on HTTP 401/403/429/432/433. |
| `TAVILY_API_URL` | `https://api.tavily.com` | Tavily API base URL. |

### Optional Fallback & Parser Providers

| Variable | Description |
|---|---|
| `EXA_API_KEY` | Exa — semantic search with native date/domain filters and page fetching. |
| `TINYFISH_API_KEY` | TinyFish — search and JS-capable page fetching. |
| `FIRECRAWL_API_KEY` / `FIRECRAWL_API_URL` | Firecrawl — last-resort fallback for search and page fetching. |
| `GITHUB_TOKEN` | Raises GitHub API rate limits and unlocks private repos for the built-in GitHub parser. |
| `LLM_SEARCH_SOURCE_PROVIDERS` | The providers to use, for both engines, as `a,b,c` or `[a, b, c]` (default: every configured one, in the order `tavily, exa, tinyfish, firecrawl, brave`). Engine `brave`: providers left out are not used, and the list sets the fallback order after Brave and Tavily (reorder whole modes with `LLM_SEARCH_QUICK_ORDER` / `LLM_SEARCH_DEEP_ORDER`). Engine `grok`: sets the order of the extra sources. |

### Grok Engine *(only needed when `LLM_SEARCH_PROVIDER=grok`)*

| Variable | Default | Description |
|---|---|---|
| `GROK_SEARCH_API_KEY` | — | API key for the gateway at `GROK_SEARCH_URL`. |
| `GROK_SEARCH_URL` | `https://api.x.ai` | Gateway base URL (xAI, or any gateway that speaks `GROK_SEARCH_PROTOCOL`). |
| `GROK_SEARCH_MODEL` | `grok-4-1-fast-reasoning` | Model name. |
| `GROK_SEARCH_PROTOCOL` | `responses` | API format at `GROK_SEARCH_URL`: `responses` (`/v1/responses`) or `chat_completions` (`/v1/chat/completions`). Key and model stay `GROK_SEARCH_API_KEY` / `GROK_SEARCH_MODEL`. Local (stdio) only; remote HTTP mode always uses `responses`. |
| `GROK_SEARCH_EXTRA_SOURCES` | `3` | Number of extra web sources to attach alongside Grok's answer. |

Grok also supports OAuth via `llm-search login` / `status` / `logout` with `GROK_SEARCH_AUTH_MODE=oauth`. This flow is inherited from upstream and may violate xAI's Terms of Service—use it at your own discretion.

### Response Limits, Caching & Timeouts

| Variable | Default | Description |
|---|---|---|
| `LLM_SEARCH_RESPONSE_MAX_CHARS` | `45000` | Max total response size for `web_search`, and the shared budget for multi-URL `web_fetch`. |
| `LLM_SEARCH_FETCH_MAX_CHARS` | *(unset)* | Max character count for a single-URL `web_fetch`. |
| `LLM_SEARCH_MAX_INLINE_SOURCES` | `5` | Max number of search results that include inline page content. |
| `LLM_SEARCH_CACHE_SIZE` | `256` | Number of search sessions kept in the in-memory cache. |
| `LLM_SEARCH_TIMEOUT_SECONDS` | `60` | HTTP timeout for all upstream API calls. |
| `LLM_SEARCH_ENRICH_CONCURRENCY` | `3` | Concurrency limit when enriching search results with page content (1–5). |
| `LLM_SEARCH_OUTPUT_DIR` | `<tmp>/llm-search` | Output directory for local `web_crawl` files (saved under `crawl/` with owner-only permissions). |
| `LLM_SEARCH_RESEARCH_MAX_WAIT_SECONDS` | *(unset)* | Max wait time for `mode=research` (defaults to 300s for `mini`, 900s for `pro` / `auto`). |

> Settings that apply to every engine use the `LLM_SEARCH_*` prefix; only Grok-specific ones use `GROK_SEARCH_*`. Every setting is listed in [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

### Shared Config File

If you use multiple MCP clients, you can keep your keys in one place instead of repeating them in every client config:

```bash
llm-search --init
```

This creates `~/.config/grok-search-rs/config.toml` using lowercase `snake_case` keys (such as `llm_search_provider` and `brave_api_key`). Unknown keys are rejected. Precedence is: **client `env` > config file > built-in defaults**.

> **Note:** This config path is shared with upstream `grok-search-rs`. If you run both binaries on the same machine, older `grok-search-rs` builds will reject new keys like `brave_api_key`. In that case, configure `llm-search` via client `env` variables instead.

---

## Remote HTTP Mode *(Optional)*

You can also deploy `llm-search` as a stateless, multi-tenant HTTP server:

```bash
cargo build --profile release-http --features http
LLM_SEARCH_BIND=127.0.0.1:8080 target/release-http/llm-search --http
```

The server stores no API keys. Callers pass their own keys per request via HTTP headers:
`X-Brave-Api-Key`, `X-Tavily-Api-Key`, `X-Exa-Api-Key`, `X-Tinyfish-Api-Key`, `X-Firecrawl-Api-Key`, `X-Grok-Api-Key`, `X-GitHub-Token`.

Put a TLS reverse proxy in front of it for production (a `Caddyfile` and `docker-compose.yml` are included in the repo).

---

## Development

Building from source requires Rust 1.78 or newer:

```bash
cargo build --release   # binary at target/release/llm-search
```

Run formatting, lints, and tests:

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features http -- -D warnings
cargo test
cargo test --features http
```

Live end-to-end probes (`tests/integration_*.rs`) are `#[ignore]`d because they call real APIs with real keys; each file's header lists the env vars, and they run with `cargo test --test <name> -- --ignored --nocapture`. Keep live logs under `logs/` (ignored by git).

Further reading:
- [docs/CONFIGURATION.md](docs/CONFIGURATION.md) — Every setting, HTTP header, and config-file key
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — How tools, engines, providers, and transports fit together
- [docs/llm-search-plan.md](docs/llm-search-plan.md) — Implementation plan and feature breakdown for this fork

---

## Credits

- **[GrokSearch-rs](https://github.com/Episkey-G/GrokSearch-rs)** (MIT, © 2026 GrokSearch-rs contributors) — Base codebase, forked at v0.1.26 (commit `3245542`). The MCP framework, provider chain, specialized parsers, response budgeting, caching, and HTTP server come from upstream.
- **[deepseek-harness-web-search-plugin](https://github.com/cloveric/deepseek-harness-web-search-plugin)** (MIT, © 2026 Cloveric) — Design inspiration for Brave/Tavily routing (`quick` / `deep` / `verify`), fallback disclosure, `source_log`, `content_hash`, `provider_status`, and key redaction. Reimplemented from scratch in Rust.

## License

MIT — see [LICENSE](LICENSE).
