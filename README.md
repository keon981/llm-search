# llm-search

給 AI coding agent（Claude Code、Codex 等）用的網路搜尋 MCP server，以 Rust 撰寫。

- **搜尋**：預設用 Brave + Tavily 路由，依 `mode` 決定先用哪家、要不要兩家並行。也可以切回 Grok 寫答案的模式。
- **抓網頁**：GitHub / StackExchange / arXiv / Wikipedia 用專門解析器（不需要 key），其他網頁走 Tavily → Exa → TinyFish → Firecrawl。
- **可追溯**：每筆結果都帶 `source_log`（`src_001` 編號）；抓網頁附 `content_hash`（SHA-256）和 `extracted_at`；改用備援服務時回傳 `fallbacks` 和 `notice`。

本專案 fork 自 [GrokSearch-rs](https://github.com/Episkey-G/GrokSearch-rs)，並移植了 [deepseek-harness-web-search-plugin](https://github.com/cloveric/deepseek-harness-web-search-plugin) 的搜尋路由與來源追蹤設計。詳見[致謝](#致謝)。

---

## 工具

| 工具 | 用途 |
|---|---|
| `web_search` | 搜尋網路。`mode`、`max_results`、`recency_days`、`include_domains`、`exclude_domains` 控制範圍 |
| `get_sources` | 取回上一次 `web_search` 快取的來源，不重新搜尋，可用 `offset` / `limit` 分頁 |
| `web_fetch` | 讀網頁全文。`url` 一頁或 `urls` 最多 10 頁；`depth`（basic / advanced）、`format`（markdown / text）、`max_chars` |
| `web_map` | 列出網站底下的網址（Tavily Map） |
| `provider_status` | 只檢查設定、不打 API、不扣額度：哪些服務已設定、目前引擎、各工具的服務順序 |
| `doctor` | 實際呼叫各服務檢查連線，回報 ok / not_configured / auth_error / rate_limited / timeout / error。會扣少量額度，`provider` 可只測一家 |

所有回傳內容與錯誤訊息都會遮蔽 API key。

### `web_search` 模式（引擎 `brave`）

| `mode` | 流程 | 適合 |
|---|---|---|
| `quick`（預設） | Brave → 失敗才用 Tavily | 一般查詢、找文件、新聞 |
| `deep` | Tavily advanced（附簡短答案與每筆最多 6,000 字網頁內容）→ 失敗才用 Brave | 研究、需要網頁內容 |
| `verify` | Brave 與 Tavily 並行，合併去重 | 重要或有爭議的說法（兩家都扣額度） |

`max_results` 1–10，預設 5。Brave 與 Tavily 都失敗時，依序改用 Exa → TinyFish → Firecrawl。

### `web_fetch` 抓取順序

1. GitHub issue / PR / release、StackExchange、arXiv、Wikipedia：專門解析器，輸出結構化 Markdown。
2. 其他網址：Tavily Extract → Exa → TinyFish → Firecrawl，第一個有內容的勝出。Tavily 用預設深度抓到空內容時（多半是用 JavaScript 產生內容的網頁），會自動改用 `advanced` 重抓一次。
3. Brave 只能搜尋，不參與抓網頁。

---

## 安裝

需要 Rust 1.78 以上。

```bash
cd llm-search
cargo build --release
```

執行檔在 `target/release/llm-search`。MCP client 的 `command` 指向它的絕對路徑。

### 註冊到 Claude Code

```bash
claude mcp add llm-search -s user \
  -e BRAVE_API_KEY=YOUR_BRAVE_KEY \
  -e TAVILY_API_KEY=YOUR_TAVILY_KEY \
  -- /absolute/path/to/llm-search/target/release/llm-search
```

註冊後設定存在 `~/.claude.json` 的 `mcpServers.llm-search.env`，可在那裡把佔位文字換成真的 key。開新 session 後先呼叫 `provider_status` 確認。

### Codex（TOML）

```toml
[mcp_servers.llm-search]
type = "stdio"
command = "/absolute/path/to/llm-search/target/release/llm-search"

[mcp_servers.llm-search.env]
BRAVE_API_KEY = ""
TAVILY_API_KEY = ""
```

---

## 設定

### 搜尋引擎

| 變數 | 預設 | 說明 |
|---|---|---|
| `LLM_SEARCH_PROVIDER` | `brave` | `web_search` 的引擎。`brave` = Brave / Tavily 路由；`grok` = Grok 寫答案加來源（`mode` 會被忽略）。不認得的值會警告並使用 `brave` |
| `BRAVE_API_KEY` | — | Brave Search API key（也接受 `BRAVE_SEARCH_API_KEY`） |
| `BRAVE_API_URL` | `https://api.search.brave.com` | Brave API base |
| `BRAVE_ENABLED` | `true` | 設 `false` 可停用 Brave |
| `TAVILY_API_KEY` | — | Tavily key。逗號分隔多把 key 會輪流使用，遇到 401/403/429/432/433 自動換下一把 |
| `TAVILY_API_URL` | `https://api.tavily.com` | Tavily API base |

引擎 `brave` 至少需要 `BRAVE_API_KEY` 或 `TAVILY_API_KEY` 其中一把；`web_map` 需要 Tavily。

### 其他來源服務（選用）

| 變數 | 說明 |
|---|---|
| `EXA_API_KEY` | Exa（語意搜尋、原生日期與網域篩選） |
| `TINYFISH_API_KEY` | TinyFish（搜尋 + 可執行 JavaScript 的抓取） |
| `FIRECRAWL_API_KEY` / `FIRECRAWL_API_URL` | Firecrawl（抓網頁最後的備援） |
| `GITHUB_TOKEN` | GitHub 解析器提高 API 額度、可讀私有 repo |

### Grok 引擎（`LLM_SEARCH_PROVIDER=grok` 才需要）

| 變數 | 預設 | 說明 |
|---|---|---|
| `GROK_SEARCH_API_KEY` | — | xAI / Grok 相容 gateway 的 key |
| `GROK_SEARCH_URL` | `https://api.x.ai` | Gateway URL |
| `GROK_SEARCH_MODEL` | `grok-4-1-fast-reasoning` | 模型 |
| `OPENAI_COMPATIBLE_API_URL` / `OPENAI_COMPATIBLE_API_KEY` / `OPENAI_COMPATIBLE_MODEL` | — | 改用 OpenAI 相容的 chat/completions gateway（不設 `GROK_SEARCH_API_KEY` 時生效） |
| `GROK_SEARCH_EXTRA_SOURCES` | `3` | Grok 答案之外另外補的來源數 |
| `GROK_SEARCH_SOURCE_PROVIDERS` | 未設定 | 補來源的服務順序，預設 `tavily, exa, tinyfish, firecrawl, brave` |

Grok 也支援 OAuth：`llm-search login` / `status` / `logout`，再設 `GROK_SEARCH_AUTH_MODE=oauth`。這個流程沿用上游的作法，可能違反 xAI 條款，使用前自行評估。

### 回應大小與快取

| 變數 | 預設 | 說明 |
|---|---|---|
| `GROK_SEARCH_RESPONSE_MAX_CHARS` | `45000` | `web_search` 整體回應字數上限，也是 `web_fetch` 多頁模式的共用上限 |
| `GROK_SEARCH_FETCH_MAX_CHARS` | 未設定 | `web_fetch` 單頁字數上限 |
| `GROK_SEARCH_MAX_INLINE_SOURCES` | `5` | 附網頁內容的來源數上限 |
| `GROK_SEARCH_CACHE_SIZE` | `256` | 快取的搜尋 session 數 |
| `GROK_SEARCH_TIMEOUT_SECONDS` | `60` | 所有上游的 HTTP 逾時 |
| `GROK_SEARCH_ENRICH_CONCURRENCY` | `3` | 補抓網頁內容的並行數（1–5） |

`GROK_SEARCH_*` 這組名稱沿用上游，暫未改名。完整清單見 [docs/CONFIGURATION.md](docs/CONFIGURATION.md)（該文件仍是上游內容，尚未加入 Brave 與路由設定）。

### 共用設定檔

`llm-search --init` 會產生 `~/.config/grok-search-rs/config.toml`，多個 MCP client 可共用一組 key。優先順序：client 的 `env` > 設定檔 > 內建預設。檔案內的 key 用小寫 snake_case（例如 `llm_search_provider`、`brave_api_key`），不認得的 key 會被拒絕。

> 路徑與上游 GrokSearch-rs 相同。若同時安裝了 grok-search-rs，舊版不認得 `brave_api_key` 等新欄位，會拒絕整個設定檔，這種情況請改用 `env` 設定。

---

## 遠端 HTTP 模式（選用）

```bash
cargo build --profile release-http --features http
GROK_MCP_BIND=127.0.0.1:8080 target/release-http/llm-search --http
```

伺服器不保存任何 key，每個請求用 header 帶自己的 key：`X-Brave-Api-Key`、`X-Tavily-Api-Key`、`X-Exa-Api-Key`、`X-Tinyfish-Api-Key`、`X-Firecrawl-Api-Key`、`X-Grok-Api-Key`、`X-GitHub-Token`。前面請放 TLS reverse proxy（repo 內附 `Caddyfile`、`docker-compose.yml`）。

---

## 開發

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo test --features http
```

- [docs/llm-search-plan.md](docs/llm-search-plan.md)：本 fork 的實作計畫與功能對照
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)、[docs/TESTING.md](docs/TESTING.md)：上游文件

---

## 致謝

- **[GrokSearch-rs](https://github.com/Episkey-G/GrokSearch-rs)**（MIT，© 2026 GrokSearch-rs contributors）：本專案的程式碼基底，fork 自 v0.1.26（commit `3245542`）。MCP 框架、來源服務鏈、專門解析器、回應預算、快取、HTTP 模式皆來自上游。
- **[deepseek-harness-web-search-plugin](https://github.com/cloveric/deepseek-harness-web-search-plugin)**（MIT，© 2026 Cloveric）：Brave / Tavily 路由模式（quick / deep / verify）、備援揭露、`source_log`、`content_hash`、`provider_status`、金鑰遮蔽等設計參考此專案，以 Rust 重新實作，未複製其程式碼。

## License

MIT — 見 [LICENSE](LICENSE)。
