# llm-search

[English](README.md) | 繁體中文

用 Rust 寫的網路搜尋與網頁抓取 MCP Server，專為 AI coding agent（Claude Code、Codex 等）設計。

- **聰明的搜尋路由**：依照查詢模式自動在 Brave 與 Tavily 之間切換——一般快速查詢、深度讀內文、交叉驗證，或是產出完整研究報告都可以。兩家都掛掉時會依序備援到 Exa、TinyFish、Firecrawl；想切回 Grok 直接幫你整理答案，也只要改一個環境變數。
- **免 Key 專屬解析 + 多層備援抓網頁**：GitHub（issue / PR / release）、StackExchange、arXiv、Wikipedia 內建專門解析器，完全不需要 API Key。其他一般網頁則依序走 Tavily → Exa → TinyFish → Firecrawl，誰先抓到內容就回傳誰。
- **來源可追溯、金鑰不外洩**：每筆搜尋結果都附 `source_log` 編號（`src_001` 等），抓取的網頁會帶 SHA-256 `content_hash` 與 `extracted_at` 時間戳；動用到備援服務時會在 `fallbacks` 與 `notice` 明確告知；所有回傳內容與錯誤訊息都會自動遮蔽 API Key。

本專案 fork 自 [GrokSearch-rs](https://github.com/Episkey-G/GrokSearch-rs)，並參考 [deepseek-harness-web-search-plugin](https://github.com/cloveric/deepseek-harness-web-search-plugin) 的搜尋路由與來源追蹤設計重新以 Rust 實作。詳見[致謝](#致謝)。

---

## 快速開始

### 1. 安裝

npm 套件已內含各平台預先編譯好的執行檔（macOS universal、Linux x64 / arm64、Windows x64 / arm64），不需要另外裝 Rust。

```bash
npm install -g @keon981/llm-search
# 或使用 Vite+：
vp install -g @keon981/llm-search

llm-search --version
```

之後要更新，再跑一次同一行安裝指令即可。

### 2. 接到你的 Agent

**Claude Code**

```bash
claude mcp add llm-search -s user \
  -e BRAVE_API_KEY=YOUR_BRAVE_KEY \
  -e TAVILY_API_KEY=YOUR_TAVILY_KEY \
  -- llm-search
```

這會把設定寫進 `~/.claude.json` 的 `mcpServers.llm-search.env`，隨時可以去那裡換成真正的 Key。重新開一個 session 後，請 Claude 跑一次 `provider_status` 確認設定正常即可。

**Codex（`config.toml`）**

```toml
[mcp_servers.llm-search]
type = "stdio"
command = "llm-search"

[mcp_servers.llm-search.env]
BRAVE_API_KEY = ""
TAVILY_API_KEY = ""
```

> 只要填 `BRAVE_API_KEY`、`TAVILY_API_KEY`、`EXA_API_KEY`、`TINYFISH_API_KEY`、`FIRECRAWL_API_KEY` **其中一把**就能開始用 `web_search`。Brave 與 Tavily 兩把都填才能發揮完整的路由模式；另外 `web_map`、`web_crawl` 與 `mode=research` 需要 Tavily Key。

---

## 工具一覽

| 工具 | 用途 |
|---|---|
| `web_search` | 搜尋網路。支援結果數量（`max_results`，1–20，預設 5）、時間範圍、網域白/黑名單、國家/語言、安全搜尋與精確比對，並提供 `quick`、`deep`、`verify`、`research` 四種模式。 |
| `get_sources` | 分頁讀取上一次 `web_search` 或遠端 `web_crawl` 快取下來的來源（`offset` / `limit`），不會重新打 API。 |
| `web_fetch` | 讀取網頁完整內容，可傳單一 `url` 或一次最多 20 個 `urls`。支援 `markdown` / `text` 格式、`basic` / `advanced` 抓取深度、字數上限（`max_chars`）與 Tavily 擷取選項。 |
| `web_map` | 透過 Tavily Map 列出網站底下的網址清單，支援深度/廣度限制、自然語言指示，以及路徑與網域的正規式篩選。 |
| `web_crawl` | 透過 Tavily Crawl 沿著連結整站抓取多頁內容。本機模式下會存成單一 Markdown 檔並回傳各頁對應行號；遠端 HTTP 模式則直接回傳內容，其餘透過 `get_sources` 分頁讀取。 |
| `provider_status` | 離線檢查設定——不打 API、不扣任何額度。列出目前設定了哪些服務、使用哪個引擎，以及各工具的備援順序。 |
| `doctor` | 實際連線測試各家已設定的服務（會消耗少量額度），回報 `ok`、`not_configured`、`auth_error`、`rate_limited`、`timeout` 或 `error`。可帶 `provider` 只測單一服務。 |

### `web_search` 搜尋模式

使用預設的 `brave` 引擎時，可依任務需求選擇不同的 `mode`：

| 模式 | 運作方式 | 適用情境 |
|---|---|---|
| `quick`（預設） | 先走 Brave → 失敗才改用 Tavily | 一般查資料、找文件、看新聞 |
| `deep` | 先走 Tavily `advanced`（附簡短摘要 + 每頁最多 6,000 字內文）→ 失敗才改用 Brave | 需要讀網頁內文、不只是看搜尋摘要時 |
| `verify` | 同時查詢 Brave 與 Tavily，合併結果並去重 | 查證重要或有爭議的說法（兩家都會扣額度） |
| `research` | 呼叫 Tavily Research 產出多來源、附引用的研究報告（不限引擎，有 Tavily Key 就能用） | 使用者明確要求做深入研究報告時 |

如果 Brave 與 Tavily 都失敗，會依序退回使用 **Exa → TinyFish → Firecrawl**。以上是 `quick` / `deep` 的預設順序，可以用 `LLM_SEARCH_QUICK_ORDER` / `LLM_SEARCH_DEEP_ORDER` 調整（見[搜尋引擎](#搜尋引擎)）；`provider_status` 會顯示實際使用的順序。

幾個使用上值得注意的細節：
- **時間篩選**：`recency_days`、`time_range`、`start_date` / `end_date` 三選一，在 Brave、Tavily 與 Exa 上都是嚴格的日期過濾。TinyFish 只支援起始日期，因此有帶 `end_date` 時會自動跳過 TinyFish。
- **Tavily 專屬參數**：像 `topic`、`search_depth`、`chunks_per_source`、`include_answer`、`include_raw_content`、`include_images`、`include_domains_mode` 這些參數，只在實際由 Tavily 回應時才會生效（`deep`、`verify` 或備援時）；其他情況會被忽略並在 `warnings` 裡註明。
- **耗時較長的研究報告（`mode=research`）**：預設會等待報告完成（`mini` 上限 300 秒、`pro` / `auto` 上限 900 秒，可用 `LLM_SEARCH_RESEARCH_MAX_WAIT_SECONDS` 調整）。超時未完成時會先回傳 `research_id` 與目前進度，之後帶著該 `research_id` 再呼叫一次即可接著等。支援 `research_model`、`output_schema`、`citation_format`、`output_length`；另外研究模式下的 `include_domains` 是「優先參考」（上限 20 個網域），不是硬性過濾。

### `web_fetch` 抓取流程

1. **內建專屬解析器（免 API Key）**：GitHub（issue / PR / release）、StackExchange、arXiv、Wikipedia 會直接解析成結構化 Markdown。
2. **一般網頁**：依序走 **Tavily Extract → Exa → TinyFish → Firecrawl**，第一個抓到有效內容的直接回傳。如果 Tavily 預設深度抓回來是空的（常見於純 JS 渲染的網頁），會自動改用 `advanced` 再試一次。
3. **批次抓取（`urls`）**：當 Tavily 排在第一順位時，所有非專屬解析的網址會合併成一次 Tavily 批次請求；Tavily 沒抓到的網址再逐一交給後面的備援服務處理，原本失敗的原因也會保留在 `failed` 欄位中。
4. Brave 僅提供搜尋功能，不參與 `web_fetch`。

---

## 設定說明

### 搜尋引擎

| 環境變數 | 預設值 | 說明 |
|---|---|---|
| `LLM_SEARCH_PROVIDER` | `brave` | `web_search` 使用的引擎。`brave` 為 Brave / Tavily 路由；`grok` 為 Grok 直接寫答案加來源（此模式下會忽略 `mode`）。填錯值會發出警告並退回 `brave`。 |
| `LLM_SEARCH_QUICK_ORDER` | `brave, tavily, exa, tinyfish, firecrawl` | `mode=quick` 的服務順序，可寫 `a,b,c` 或 `[a, b, c]`。列出的服務照清單排在前面，沒列到的照預設順序接在後面。 |
| `LLM_SEARCH_DEEP_ORDER` | `tavily, brave, exa, tinyfish, firecrawl` | `mode=deep` 的服務順序，規則同上。 |
| `BRAVE_API_KEY` | — | Brave Search API Key（也接受 `BRAVE_SEARCH_API_KEY`）。 |
| `BRAVE_API_URL` | `https://api.search.brave.com` | Brave API 網址。 |
| `BRAVE_ENABLED` | `true` | 設為 `false` 可停用 Brave。 |
| `TAVILY_API_KEY` | — | Tavily API Key。支援用逗號分隔多把 Key 輪流使用，遇到 HTTP 401/403/429/432/433 會自動切換下一把。 |
| `TAVILY_API_URL` | `https://api.tavily.com` | Tavily API 網址。 |

### 其他備援與解析服務（選用）

| 環境變數 | 說明 |
|---|---|
| `EXA_API_KEY` | Exa — 語意搜尋（支援原生日期與網域篩選）及網頁抓取。 |
| `TINYFISH_API_KEY` | TinyFish — 搜尋與可執行 JavaScript 的網頁抓取。 |
| `FIRECRAWL_API_KEY` / `FIRECRAWL_API_URL` | Firecrawl — 搜尋與抓網頁的最後一層備援。 |
| `GITHUB_TOKEN` | 提高內建 GitHub 解析器的 API 額度，並允許讀取私有 repo。 |
| `LLM_SEARCH_SOURCE_PROVIDERS` | 要使用的服務，兩種引擎都會讀，可寫 `a,b,c` 或 `[a, b, c]`（預設：所有已設定的服務，依 `tavily, exa, tinyfish, firecrawl, brave` 排序）。引擎 `brave`：沒列出的服務不會用到，清單也決定 Brave、Tavily 之後的備援順序（整個模式的順序用 `LLM_SEARCH_QUICK_ORDER` / `LLM_SEARCH_DEEP_ORDER` 調整）。引擎 `grok`：決定補充來源的順序。 |

### Grok 引擎（僅在 `LLM_SEARCH_PROVIDER=grok` 時需要）

| 環境變數 | 預設值 | 說明 |
|---|---|---|
| `GROK_SEARCH_API_KEY` | — | `GROK_SEARCH_URL` 那個 gateway 的 API Key。 |
| `GROK_SEARCH_URL` | `https://api.x.ai` | Gateway 網址（xAI，或任何支援 `GROK_SEARCH_PROTOCOL` 格式的 gateway）。 |
| `GROK_SEARCH_MODEL` | `grok-4-1-fast-reasoning` | 使用的模型名稱。 |
| `GROK_SEARCH_PROTOCOL` | `responses` | `GROK_SEARCH_URL` 使用的 API 格式：`responses`（`/v1/responses`）或 `chat_completions`（`/v1/chat/completions`）。key 和模型一樣用 `GROK_SEARCH_API_KEY` / `GROK_SEARCH_MODEL`。只在本機（stdio）生效；遠端 HTTP 模式固定用 `responses`。 |
| `GROK_SEARCH_EXTRA_SOURCES` | `3` | Grok 答案之外額外補充的來源數量。 |

Grok 也支援 OAuth 登入（執行 `llm-search login` / `status` / `logout` 並設定 `GROK_SEARCH_AUTH_MODE=oauth`）。這個機制沿用自上游專案，可能違反 xAI 使用條款，使用前請自行評估風險。

### 回應上限、快取與逾時

| 環境變數 | 預設值 | 說明 |
|---|---|---|
| `LLM_SEARCH_RESPONSE_MAX_CHARS` | `45000` | `web_search` 整體回應的字數上限，同時也是 `web_fetch` 多頁模式的共用額度。 |
| `LLM_SEARCH_FETCH_MAX_CHARS` | 未設定 | `web_fetch` 單頁模式的字數上限。 |
| `LLM_SEARCH_MAX_INLINE_SOURCES` | `5` | 搜尋結果中最多幾筆附帶網頁內文。 |
| `LLM_SEARCH_CACHE_SIZE` | `256` | 記憶體中保留的搜尋 session 快取數量。 |
| `LLM_SEARCH_TIMEOUT_SECONDS` | `60` | 所有上游 API 請求的 HTTP 逾時秒數。 |
| `LLM_SEARCH_ENRICH_CONCURRENCY` | `3` | 補抓網頁內文時的並行數（1–5）。 |
| `LLM_SEARCH_OUTPUT_DIR` | 系統暫存目錄下的 `llm-search` | 本機 `web_crawl` 輸出檔的存放位置（實際存在其下的 `crawl/`，權限僅限擁有者讀寫）。 |
| `LLM_SEARCH_RESEARCH_MAX_WAIT_SECONDS` | 未設定 | `mode=research` 的最長等待秒數（未設時 `mini` 為 300 秒，`pro` / `auto` 為 900 秒）。 |

> 所有引擎共用的設定使用 `LLM_SEARCH_*` 前綴，只有 Grok 專用的設定使用 `GROK_SEARCH_*`。所有設定的完整清單見 [docs/CONFIGURATION.md](docs/CONFIGURATION.md)。

### 共用設定檔

如果同時使用多個 MCP client，不想每個設定檔都重複貼一次 API Key，可以建立共用設定檔：

```bash
llm-search --init
```

這會產生 `~/.config/grok-search-rs/config.toml`，欄位名稱使用小寫 `snake_case`（例如 `llm_search_provider`、`brave_api_key`），不認得的欄位會直接報錯拒絕。優先順序為：**client 的 `env` > 設定檔 > 內建預設**。

> **注意：** 這個路徑與上游的 `grok-search-rs` 共用。如果你的電腦同時有裝舊版 `grok-search-rs`，它會因為不認識 `brave_api_key` 等新欄位而拒絕整份設定檔；遇到這種情況請改用各 client 自己的 `env` 設定。

---

## 遠端 HTTP 模式（選用）

除了本機 stdio，也可以把 `llm-search` 編譯成無狀態的多租戶 HTTP 伺服器：

```bash
cargo build --profile release-http --features http
LLM_SEARCH_BIND=127.0.0.1:8080 target/release-http/llm-search --http
```

伺服器本身不儲存任何 API Key，改由每個請求透過 HTTP Header 自帶：
`X-Brave-Api-Key`、`X-Tavily-Api-Key`、`X-Exa-Api-Key`、`X-Tinyfish-Api-Key`、`X-Firecrawl-Api-Key`、`X-Grok-Api-Key`、`X-GitHub-Token`。

正式環境請在前方加上 TLS reverse proxy（repo 內已附 `Caddyfile` 與 `docker-compose.yml` 範例）。

---

## 開發

從原始碼自行編譯需要 Rust 1.78 以上版本：

```bash
cargo build --release   # 產出執行檔位於 target/release/llm-search
```

格式檢查、Lint 與測試：

```bash
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo clippy --all-targets --features http -- -D warnings
cargo test
cargo test --features http
```

線上端對端測試（`tests/integration_*.rs`）會用真的 key 呼叫真的 API，所以預設 `#[ignore]`；需要的環境變數寫在各檔案開頭，用 `cargo test --test <名稱> -- --ignored --nocapture` 執行。線上測試的 log 放在 `logs/`（已被 git 忽略）。

延伸閱讀：
- [docs/CONFIGURATION.md](docs/CONFIGURATION.md)：所有設定、HTTP header 與設定檔鍵名
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md)：工具、引擎、服務與傳輸方式如何組在一起
- [docs/llm-search-plan.md](docs/llm-search-plan.md)：本 fork 的實作計畫與功能對照

---

## 致謝

- **[GrokSearch-rs](https://github.com/Episkey-G/GrokSearch-rs)**（MIT，© 2026 GrokSearch-rs contributors）：本專案的程式碼基底，fork 自 v0.1.26（commit `3245542`）。MCP 框架、備援服務鏈、專屬解析器、回應字數控制、快取與 HTTP 模式均來自上游。
- **[deepseek-harness-web-search-plugin](https://github.com/cloveric/deepseek-harness-web-search-plugin)**（MIT，© 2026 Cloveric）：Brave / Tavily 路由模式（`quick` / `deep` / `verify`）、備援揭露、`source_log`、`content_hash`、`provider_status` 與金鑰遮蔽等設計靈感來源，全部以 Rust 重新實作，未複製其程式碼。

## License

MIT — 詳見 [LICENSE](LICENSE)。

