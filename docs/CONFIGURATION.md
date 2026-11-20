# Configuration

The complete reference for every setting llm-search reads. The [README](../README.md#configuration) lists the common ones.

llm-search merges configuration from three layers:

1. **Process environment variables** (highest — what your MCP client passes in `env`).
2. **TOML config file** — `$LLM_SEARCH_CONFIG` if set, otherwise `<home>/.config/grok-search-rs/config.toml`. `<home>` is `$HOME` on Unix / Git Bash, `%USERPROFILE%` on native Windows shells (PowerShell, cmd). See [Config file](#config-file).
3. **Built-in defaults** (lowest).

**Naming.** A setting is named after what it belongs to: llm-search's own settings use `LLM_SEARCH_*`, a provider's settings use that provider's name (`BRAVE_*`, `TAVILY_*`, `EXA_*`, `TINYFISH_*`, `FIRECRAWL_*`), and the Grok engine's settings use `GROK_SEARCH_*`.

Booleans accept `1`, `true`, or `yes` as enabled; any other value is disabled.

## Configuration channels (stdio env vs remote headers)

The MCP transport decides how configuration arrives:

- **stdio (local):** the MCP client starts `llm-search` as a child process and passes **environment variables** (the `env` block of your client config).
- **Streamable HTTP (remote):** the client talks to a running, multi-tenant server that stores **no** credentials. Each request carries its keys as **HTTP headers**; server-side env is shared by every caller, so per-request secrets are never read from it.

Both carry the same values. This document uses env names; on the remote transport, send the matching header:

| Setting | stdio env key | remote HTTP header |
|---|---|---|
| Brave API key | `BRAVE_API_KEY` | `X-Brave-Api-Key` |
| Tavily API key | `TAVILY_API_KEY` | `X-Tavily-Api-Key` |
| Exa API key | `EXA_API_KEY` | `X-Exa-Api-Key` |
| TinyFish API key | `TINYFISH_API_KEY` | `X-Tinyfish-Api-Key` |
| Firecrawl API key | `FIRECRAWL_API_KEY` | `X-Firecrawl-Api-Key` |
| GitHub token | `GITHUB_TOKEN` | `X-GitHub-Token` |
| Grok API key | `GROK_SEARCH_API_KEY` | `X-Grok-Api-Key` |
| Grok gateway URL | `GROK_SEARCH_URL` | `X-Grok-Base-Url` (HTTPS only) |
| Grok model | `GROK_SEARCH_MODEL` | `X-Grok-Model` |

Only these nine are accepted as headers. Everything else (engine choice, orders, timeouts, budgets, provider base URLs, feature toggles) is **operator-fixed**: set once in the server's environment. `GROK_SEARCH_URL` / `GROK_SEARCH_MODEL` also have operator defaults that the headers override per request.

Over HTTP the server strips these from its own environment at startup, so they never apply to a request: every API key above, `BRAVE_SEARCH_API_KEY`, `GROK_SEARCH_PROTOCOL`, and OAuth (`GROK_SEARCH_AUTH_MODE` / `GROK_SEARCH_AUTH_FILE`). The remote server therefore speaks only the Grok **Responses** protocol and never uses OAuth; both other options are stdio-only.

Header names are not derived mechanically (`GROK_SEARCH_URL` → `X-Grok-Base-Url`); read them off the table.

### Minimal setup — each transport

Local (stdio):

```bash
claude mcp add llm-search -s user \
  -e BRAVE_API_KEY=... \
  -e TAVILY_API_KEY=... \
  -- llm-search
```

Remote — headers:

```bash
claude mcp add --transport http llm-search https://<host>/mcp \
  --header "X-Brave-Api-Key: ..." \
  --header "X-Tavily-Api-Key: tvly-..."
```

## Search engine

| Variable | Default | Description |
|---|---|---|
| `LLM_SEARCH_PROVIDER` | `brave` | Engine for `web_search`. `brave`: route across search providers by `mode`. `grok`: the Grok engine writes the answer and the source chain adds sources (`mode` is ignored, except `research`). Unknown values log a warning and use `brave`. |
| `LLM_SEARCH_QUICK_ORDER` | unset | `mode=quick` provider order, as `a,b,c` or `[a, b, c]`. Listed providers go first in that order; unlisted configured ones follow in the default order (Brave, Tavily, then the source chain). Listed but unconfigured providers are skipped. Unknown names fail at startup. |
| `LLM_SEARCH_DEEP_ORDER` | unset | Same for `mode=deep` (default Tavily, Brave, then the source chain). |

`mode=verify` always runs Brave and Tavily in parallel, then the rest of the chain if both fail. `mode=research` uses Tavily Research on either engine. `provider_status` shows the order in effect for each mode.

## Brave

| Variable | Default | Description |
|---|---|---|
| `BRAVE_API_KEY` | unset | Brave Search API key. `BRAVE_SEARCH_API_KEY` is read when `BRAVE_API_KEY` is unset. |
| `BRAVE_API_URL` | `https://api.search.brave.com` | Brave API base URL. |
| `BRAVE_ENABLED` | `true` | Set to `false` to disable Brave even if a key is configured. |

Brave is search-only: it never serves `web_fetch`, `web_map`, or `web_crawl`.

## Grok engine

Used when `LLM_SEARCH_PROVIDER=grok`.

| Variable | Default | Description |
|---|---|---|
| `GROK_SEARCH_AUTH_MODE` | `api_key` | `api_key` uses `GROK_SEARCH_API_KEY`; `oauth` uses the local token file created by `llm-search login`. |
| `GROK_SEARCH_API_KEY` | required in `api_key` mode | Bearer token for the gateway at `GROK_SEARCH_URL`. |
| `GROK_SEARCH_AUTH_FILE` | `<home>/.config/grok-search-rs/auth.json` | OAuth token file override. |
| `GROK_SEARCH_URL` | `https://api.x.ai` | Root URL, `/v1` base URL, or endpoint-like URL; normalized to a `/v1` base. |
| `GROK_SEARCH_MODEL` | `grok-4-1-fast-reasoning` | Model sent in every request, whichever protocol. |
| `GROK_SEARCH_PROTOCOL` | `responses` | API format at `GROK_SEARCH_URL`: `responses` (`/v1/responses`) or `chat_completions` (`/v1/chat/completions`, for gateways that only speak that format; citations are read from OpenAI annotations, Perplexity-style citations, top-level `search_sources`, and inline `[[n]](url)`). OAuth always uses `responses`. Unknown values log a warning and use `responses`. stdio only. |
| `GROK_SEARCH_WEB_SEARCH` | `true` | Sends the `{"type":"web_search"}` tool. |
| `GROK_SEARCH_X_SEARCH` | `false` | Sends `{"type":"x_search"}` when enabled (`responses` only). |
| `GROK_SEARCH_EXTRA_SOURCES` | `3` | Supplemental sources added after a verifiable Grok answer, served by the first source-chain provider with results. `0` disables them. |
| `GROK_SEARCH_FALLBACK_SOURCES` | `5` | Sources cached from the source chain when the Grok answer is unverifiable. |

Example — calls `https://api.x.ai/v1/responses`:

```bash
LLM_SEARCH_PROVIDER=grok
GROK_SEARCH_API_KEY=...
GROK_SEARCH_URL=https://api.x.ai
GROK_SEARCH_MODEL=grok-4-1-fast-reasoning
```

### OAuth mode

OAuth mode keeps the Responses payload and changes only where the Bearer token comes from. The binary handles login and MCP stdio; it does not start a background proxy.

```bash
llm-search login
llm-search status
llm-search logout
```

`login` opens xAI OAuth in a browser, listens once on `http://127.0.0.1:56121/callback`, and writes `access_token`, `refresh_token`, `id_token`, `token_endpoint`, `base_url`, and `last_refresh` to `auth.json`. `status` prints token presence, expiry, and the auth file path without printing the token. `logout` removes the auth file.

OAuth mode reuses Hermes' xAI OAuth client id. This may violate xAI terms or create account risk, and Windows stores the token as a normal local file. Do not share the token file.

## Tavily

| Variable | Default | Description |
|---|---|---|
| `TAVILY_API_KEY` | unset | Enables Tavily search, extract, map, crawl, and research. Accepts one key or a comma-separated list (`tvly-a,tvly-b`); multiple keys rotate round-robin per request, failing over to the next key on key-scoped errors (HTTP 401/403/429/432/433). |
| `TAVILY_API_URL` | `https://api.tavily.com` | Tavily API base URL. |
| `TAVILY_ENABLED` | `true` | Set to `false` to disable Tavily even if a key is configured. |
| `LLM_SEARCH_OUTPUT_DIR` | `<OS temp>/llm-search` | Directory for `web_crawl` result files (written under `crawl/`, owner-only permissions). Only the stdio server writes files; the HTTP transport returns crawl pages inline. |
| `LLM_SEARCH_RESEARCH_MAX_WAIT_SECONDS` | unset | Wait limit for `web_search` `mode=research`. Unset: 300 s for `research_model=mini`, 900 s for `pro` / `auto`. Past the limit the call returns the `research_id` to resume with. |

`web_map` and `web_crawl` always use Tavily when `TAVILY_API_KEY` is configured, even when `LLM_SEARCH_SOURCE_PROVIDERS` leaves Tavily out.

## Firecrawl

| Variable | Default | Description |
|---|---|---|
| `FIRECRAWL_API_KEY` | unset | Enables Firecrawl search and scrape as fallbacks. |
| `FIRECRAWL_API_URL` | `https://api.firecrawl.dev` | Firecrawl API base URL. Defaults to `/v2`; an explicit `/v1` or `/v2` is preserved. |
| `FIRECRAWL_ENABLED` | `true` | Set to `false` to disable Firecrawl even if a key is configured. |

Firecrawl uses [`POST /v2/search`](https://docs.firecrawl.dev/api-reference/endpoint/search) and `POST /v2/scrape` by default. Search reads web results from the v2 `data.web` response; `data` arrays and flat `results` arrays are also accepted. For a gateway mounted at `http://localhost:9010/firecrawl`, set `FIRECRAWL_API_URL=http://localhost:9010/firecrawl` or `http://localhost:9010/firecrawl/v2`; both send requests to `/firecrawl/v2/search` and `/firecrawl/v2/scrape`. For a v1 deployment, set a base ending in `/v1`.

Firecrawl search cannot honor domain or date filters, so it is skipped for filtered requests.

## TinyFish

Search and fetch APIs built for agents (no credits consumed; rate limits apply — 30 req/min on the free plan, and the account needs Search API access). Search results are keyword-ranked; fetch renders JS-heavy pages and extracts PDFs.

| Variable | Default | Description |
|---|---|---|
| `TINYFISH_API_KEY` | unset | Enables TinyFish search and fetch. One key serves both endpoints. |
| `TINYFISH_SEARCH_API_URL` | `https://api.search.tinyfish.ai` | Search endpoint (GET). |
| `TINYFISH_FETCH_API_URL` | `https://api.fetch.tinyfish.ai` | Fetch endpoint (POST). |
| `TINYFISH_ENABLED` | `true` | Set to `false` to disable TinyFish even if a key is configured. |

Domain filters map to TinyFish's `include_domains` / `exclude_domains`. The date window maps to `recency_minutes`, which only has a lower bound, so TinyFish is skipped when `end_date` is set.

## Exa

Semantic (embeddings-first) search with native domain and published-date filters — strong on descriptive queries, papers, and official-domain discovery. Paid per request.

| Variable | Default | Description |
|---|---|---|
| `EXA_API_KEY` | unset | Enables Exa search and `/contents` fetch. |
| `EXA_API_URL` | `https://api.exa.ai` | Exa API base URL. |
| `EXA_ENABLED` | `true` | Set to `false` to disable Exa even if a key is configured. |

## Source chain

The source chain is the ordered list of configured providers. Search fallback (after Brave and Tavily in `quick` / `deep`, or after Grok in the Grok engine) and generic `web_fetch` walk it in order; the first provider with usable output wins. Providers that cannot honor a filter are skipped for that request. The whole walk shares one deadline (`LLM_SEARCH_TIMEOUT_SECONDS`), so a slow provider cannot multiply the budget by the chain length.

| Variable | Default | Description |
|---|---|---|
| `LLM_SEARCH_SOURCE_PROVIDERS` | unset | The providers to use and their chain order, as `a,b,c` or `[a, b, c]` (valid names: `tavily`, `exa`, `tinyfish`, `firecrawl`, `brave`). Providers left out are not used by either engine. Unset = every configured provider in the order `tavily, exa, tinyfish, firecrawl, brave`. Unknown names fail at startup. |

**Providers and specialist extractors are different things.** A provider (Brave, Tavily, Exa, TinyFish, Firecrawl) is an external service behind an API key. A specialist extractor (GitHub, StackExchange, arXiv, Wikipedia) is a key-free parser for one family of URLs; it is never configured and never part of the chain. With **no provider configured**, `web_fetch` still reads those four families and fails on every other URL.

## Timeouts and cache

| Variable | Default | Description |
|---|---|---|
| `LLM_SEARCH_TIMEOUT_SECONDS` | `60` | HTTP timeout for upstream requests. `web_map` / `web_crawl` use their own `timeout` argument (default 150 s) plus a 10 s margin. |
| `LLM_SEARCH_CACHE_SIZE` | `256` | Maximum cached search sessions for `get_sources`. |
| `LLM_SEARCH_FETCH_MAX_CHARS` | unset | Default character cap on single-URL `web_fetch` content. Overridden per call by `max_chars`. Unset means no truncation. |

## Source extraction

Specialist `web_fetch` extractors and `web_search` inline enrichment. The specialists call public APIs directly — no provider key required.

| Variable | Default | Description |
|---|---|---|
| `GITHUB_TOKEN` | unset | GitHub token for issue/PR/release fetches. Anonymous works but is capped at ~60 req/hr; a token raises the limit and allows private repos. |
| `LLM_SEARCH_SOURCE_MAX_ANSWERS` | `5` | StackExchange answers rendered before the "more answers" fold. |
| `LLM_SEARCH_SOURCE_MAX_COMMENTS` | `30` | GitHub / StackExchange comments rendered before folding. |
| `LLM_SEARCH_ENRICH_CONCURRENCY` | `3` | Parallel source enrichments when `web_search` is called with `include_content: true`. Clamped to `1..=5`. |
| `LLM_SEARCH_ENRICH_MAX_CHARS` | `15000` | Character cap per enriched source body. |

## Response budget

Caps the size of one `web_search` response so large source sets stay within MCP client context limits. The session cache keeps full content; truncated sources carry a note pointing at `web_fetch(url)` / `get_sources(session_id)`.

| Variable | Default | Description |
|---|---|---|
| `LLM_SEARCH_MAX_INLINE_SOURCES` | `5` | Maximum sources that carry inline `content` per `web_search` response; the rest return metadata only. |
| `LLM_SEARCH_RESPONSE_MAX_CHARS` | `45000` | Whole-response character budget (answer + per-source metadata and inline content), and the shared budget of a multi-URL `web_fetch`. Over-budget responses truncate inline content tail-first, then drop trailing sources (always keeping at least one) and set `truncated: true`. |

## Remote HTTP server

Requires a build with `--features http`.

| Variable | Default | Description |
|---|---|---|
| `LLM_SEARCH_BIND` | `127.0.0.1:8080` | Address to listen on. Setting it starts the HTTP transport even without `--http` / `serve`. |
| `LLM_SEARCH_ALLOWED_ORIGINS` | unset | Comma-separated browser origins allowed to call the server. Unset: no origin restriction (keys are per request). |

The server listens on `/mcp`. Put a TLS reverse proxy in front of it for production; the repo's `Caddyfile` also serves it under `/groksearch_rs/mcp`.

## Config file

Put a TOML file at `<home>/.config/grok-search-rs/config.toml` (or the path in `LLM_SEARCH_CONFIG`) to set values once instead of in every client's `env` block. Process env still wins.

- **macOS / Linux**: `$HOME/.config/grok-search-rs/config.toml`
- **Windows (PowerShell / cmd)**: `%USERPROFILE%\.config\grok-search-rs\config.toml`
- **Windows (Git Bash / MSYS)**: same as Unix

`llm-search --init` writes an annotated template at that path with **every key commented out**, so it changes nothing until you uncomment lines. Re-running it is a no-op when the file exists.

**Parsing is strict: one unknown key voids the whole file**, so a typo surfaces as an error instead of silently applying half your settings. MCP clients hide stderr, so the `doctor` tool reports the file's resolved path and whether it was `absent`, `loaded`, or `rejected` (with the reason).

TOML keys use lowercase `snake_case`; env names use `UPPER_CASE`. The names are not derived mechanically — use this table:

| TOML key | Env equivalent |
|---|---|
| `llm_search_provider` | `LLM_SEARCH_PROVIDER` |
| `quick_order` | `LLM_SEARCH_QUICK_ORDER` |
| `deep_order` | `LLM_SEARCH_DEEP_ORDER` |
| `source_providers` | `LLM_SEARCH_SOURCE_PROVIDERS` |
| `brave_api_key` | `BRAVE_API_KEY` |
| `brave_api_url` | `BRAVE_API_URL` |
| `brave_enabled` | `BRAVE_ENABLED` |
| `tavily_api_key` | `TAVILY_API_KEY` |
| `tavily_api_url` | `TAVILY_API_URL` |
| `tavily_enabled` | `TAVILY_ENABLED` |
| `exa_api_key` | `EXA_API_KEY` |
| `exa_api_url` | `EXA_API_URL` |
| `exa_enabled` | `EXA_ENABLED` |
| `tinyfish_api_key` | `TINYFISH_API_KEY` |
| `tinyfish_search_api_url` | `TINYFISH_SEARCH_API_URL` |
| `tinyfish_fetch_api_url` | `TINYFISH_FETCH_API_URL` |
| `tinyfish_enabled` | `TINYFISH_ENABLED` |
| `firecrawl_api_key` | `FIRECRAWL_API_KEY` |
| `firecrawl_api_url` | `FIRECRAWL_API_URL` |
| `firecrawl_enabled` | `FIRECRAWL_ENABLED` |
| `github_token` | `GITHUB_TOKEN` |
| `grok_api_key` | `GROK_SEARCH_API_KEY` |
| `grok_api_url` | `GROK_SEARCH_URL` |
| `grok_model` | `GROK_SEARCH_MODEL` |
| `grok_protocol` | `GROK_SEARCH_PROTOCOL` |
| `grok_auth_mode` | `GROK_SEARCH_AUTH_MODE` |
| `grok_auth_file` | `GROK_SEARCH_AUTH_FILE` |
| `web_search_enabled` | `GROK_SEARCH_WEB_SEARCH` |
| `x_search_enabled` | `GROK_SEARCH_X_SEARCH` |
| `default_extra_sources` | `GROK_SEARCH_EXTRA_SOURCES` |
| `fallback_sources` | `GROK_SEARCH_FALLBACK_SOURCES` |
| `timeout_seconds` | `LLM_SEARCH_TIMEOUT_SECONDS` |
| `cache_size` | `LLM_SEARCH_CACHE_SIZE` |
| `fetch_max_chars` | `LLM_SEARCH_FETCH_MAX_CHARS` |
| `output_dir` | `LLM_SEARCH_OUTPUT_DIR` |
| `research_max_wait_seconds` | `LLM_SEARCH_RESEARCH_MAX_WAIT_SECONDS` |
| `source_max_answers` | `LLM_SEARCH_SOURCE_MAX_ANSWERS` |
| `source_max_comments` | `LLM_SEARCH_SOURCE_MAX_COMMENTS` |
| `enrich_concurrency` | `LLM_SEARCH_ENRICH_CONCURRENCY` |
| `enrich_max_chars` | `LLM_SEARCH_ENRICH_MAX_CHARS` |
| `max_inline_sources` | `LLM_SEARCH_MAX_INLINE_SOURCES` |
| `response_max_chars` | `LLM_SEARCH_RESPONSE_MAX_CHARS` |

`LLM_SEARCH_CONFIG`, `LLM_SEARCH_BIND`, and `LLM_SEARCH_ALLOWED_ORIGINS` are env-only.

Example — minimal file:

```toml
brave_api_key  = "BSA..."
tavily_api_key = "tvly-..."
```

Example — Grok engine with OAuth:

```toml
llm_search_provider = "grok"
grok_auth_mode      = "oauth"
grok_model          = "grok-4.3"
```

Example — Grok engine through a chat-completions gateway:

```toml
llm_search_provider = "grok"
grok_protocol       = "chat_completions"
grok_api_url        = "https://gateway.example/v1"
grok_api_key        = "sk-..."
grok_model          = "grok-4-1-fast-reasoning"
```
