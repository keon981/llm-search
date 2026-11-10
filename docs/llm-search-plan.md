# llm-search 實作計畫

> 狀態：待確認（確認前不動程式碼）
> 基底：GrokSearch-rs 0.1.26（commit `3245542`，MIT 授權）
> 功能來源：deepseek-harness-web-search-plugin（commit `0b1c1b6`，MIT 授權）

## 1. 目標

以 GrokSearch-rs 為基底，把 deepseek-harness-web-search-plugin 的功能做進去，成為給 Claude Code 用的 MCP server：llm-search。

- `web_search` 預設用 **Brave / Tavily 分流**（DeepSeek plugin 的做法），提供 `quick`、`deep`、`verify` 三種模式。
- Grok 保留，使用者用環境變數切換；預設是 brave。
- DeepSeek plugin 的來源追蹤（sourceLog、contentHash、備援通知）、`provider_status`、`health_check`、金鑰遮蔽全部做進來。
- GrokSearch-rs 原有的強項保留：多來源抓網頁、GitHub/StackExchange/arXiv/Wikipedia 專用解析、`web_map`、`get_sources`。
- 不需要包成 plugin：用 `claude mcp add` 註冊即可。

## 2. 功能對照：DeepSeek plugin → llm-search

| DeepSeek plugin | llm-search 的做法 | 說明 |
|---|---|---|
| `web_search` 的 `quick` 模式：先 Brave，失敗改 Tavily | 同 | 之後還能再退到 Exa → TinyFish → Firecrawl（有設 key 才會用） |
| `web_search` 的 `deep` 模式：先 Tavily（advanced 深度，附 Tavily 答案和整頁內容），失敗改 Brave | 同 | 同上，最後一樣可退到其他來源 |
| `web_search` 的 `verify` 模式：Brave 和 Tavily 同時搜，網址去重後合併 | 同 | 兩家都成功時 provider 標 `mixed` |
| `maxResults` 1–10，預設 5 | 同 | |
| 備援時回傳 `fallbacks`（哪家失敗、原因）和 `notice`（要求回答時揭露） | 同 | 原本的 `fallback_used`、`fallback_reason` 保留 |
| `sourceLog`：每筆結果有 `src_001` 編號、query、provider、網址、網域、標題、摘要、排名、存取時間 | 同 | |
| `web_extract`：Tavily Extract，可給多個網址、depth、format、總字數上限，回傳 `contentHash`、`extractedAt`、`sourceLog` | **併入現有的 `web_fetch`** | GrokSearch-rs 的 `web_fetch` 功能比較多：Tavily 失敗還能改用 Exa/TinyFish/Firecrawl，也有專用解析。把多網址、depth、format、contentHash、sourceLog 補進去，不另開一個重複的工具 |
| `provider_status`：只看 key 有沒有設，不打 API、不耗額度 | 新增同名工具 | |
| `health_check`：實際打 API，結果分成 ok / 未設定 / 認證錯誤 / 被限流 / 逾時 / 其他錯誤 | **併入現有的 `doctor`** | `doctor` 本來就會實際測試各來源；補上 Brave、相同的狀態分類、可指定只測某一家 |
| 所有輸出遮蔽金鑰（Bearer、X-Subscription-Token、`*_API_KEY=`、實際 key 值） | 同 | 實作時先確認 GrokSearch-rs 有沒有現成機制，沒有就補 |
| 失敗時提示「改用內建搜尋並告知使用者」 | 改成提示用 Claude Code 內建的 WebSearch | |
| 使用規則（給了網址先抽取、搜尋用來找資料、保留來源和備援通知） | 寫進各工具的說明文字 | DeepSeek 是注入 system prompt；Claude Code 從工具說明讀規則，不需要另外的 skill |
| `BRAVE_API_KEY`（別名 `BRAVE_SEARCH_API_KEY`）、`TAVILY_API_KEY` | 同 | |
| 從 Codex `config.toml` 補讀 key | **不做** | Claude Code 用 MCP 設定的 env 傳 key |
| TaroCub、`/tarocub`、DeepSeek Harness 安裝設定 | **不做** | 跟 Claude Code 無關 |

## 3. 改完後搜尋怎麼走

```
web_search(query, mode = quick | deep | verify, max_results)
 └─ 看 LLM_SEARCH_PROVIDER（預設 brave）
     ├─ brave（預設，Brave / Tavily 分流）
     │   ├─ quick  → Brave → Tavily → Exa → TinyFish → Firecrawl
     │   ├─ deep   → Tavily(advanced) → Brave → Exa → TinyFish → Firecrawl
     │   └─ verify → Brave + Tavily 同時搜，合併去重
     └─ grok → 跟現在一樣：Grok 寫答案，其他來源補充（mode 參數不作用，回傳中註明）

web_fetch(url 或 urls, depth, format, max_chars)
 └─ Tavily → Exa → TinyFish → Firecrawl（專用網站走專用解析），附 contentHash、sourceLog
web_map / get_sources → 跟現在一樣
provider_status → 只檢查設定，不打 API
doctor → 實際測試各來源（含 Brave），狀態分類同 DeepSeek 的 health_check
```

### 回傳內容

| 欄位 | brave 模式 | grok 模式 |
|---|---|---|
| `content` | 編號結果清單；`deep` 模式最前面附 Tavily 的答案 | Grok 寫的答案 |
| `sources` | 搜尋結果（結構化） | Grok 引用的來源加上補充來源 |
| `sourceLog` | 有 | 有 |
| `fallbacks` / `notice` | 有備援時才出現 | 有備援時才出現 |
| 需要 Grok 的 key | 不需要 | 需要 |

## 4. 環境變數

**新增**

| 變數 | 預設值 | 用途 |
|---|---|---|
| `LLM_SEARCH_PROVIDER` | `brave` | `brave`（Brave / Tavily 分流）或 `grok` |
| `BRAVE_API_KEY` | — | Brave Search API 金鑰（`BRAVE_SEARCH_API_KEY` 視為同一個） |
| `BRAVE_API_URL` | `https://api.search.brave.com` | Brave API 位址 |
| `BRAVE_ENABLED` | `true` | 跟其他來源一樣，可以單獨關掉 |

**不變**：現有的 `GROK_SEARCH_*`、`OPENAI_COMPATIBLE_*`、`TAVILY_*`、`EXA_*`、`TINYFISH_*`、`FIRECRAWL_*` 全部沿用。

> 注意：原本就有 `GROK_SEARCH_WEB_SEARCH`，它是開關「Grok 內建的搜尋工具」，跟 `LLM_SEARCH_PROVIDER` 不是同一件事，名稱不動。

## 5. 要改的地方

| # | 改什麼 | 白話說明 |
|---|---|---|
| 1 | 新增 Brave 搜尋來源 | 呼叫 Brave Web Search API（跟 DeepSeek plugin 用同一個 endpoint）。時間範圍、限定網域的參數實作時先查 Brave 官方文件 |
| 2 | 來源清單加入 Brave，加「能不能抓網頁」標記 | Brave 只能搜，`web_fetch`、`web_map` 跳過它 |
| 3 | Tavily 搜尋支援 deep 參數 | deep / verify 用 advanced 深度、附答案和整頁內容（每筆整頁內容截到 6,000 字，同 DeepSeek） |
| 4 | `web_search` 加 `mode`、`max_results` 和分流邏輯 | 依第 3 節的路線 |
| 5 | 回傳加 `sourceLog`、`fallbacks`、`notice` | 依第 2 節 |
| 6 | `web_fetch` 加多網址、depth、format、contentHash、sourceLog | 吸收 DeepSeek 的 `web_extract` |
| 7 | 新增 `provider_status` | 只看設定 |
| 8 | `doctor` 加 Brave、狀態分類、可指定來源 | 吸收 DeepSeek 的 `health_check` |
| 9 | 金鑰遮蔽 | 所有工具輸出和錯誤訊息 |
| 10 | 工具說明文字寫入使用規則 | 給了網址先 `web_fetch`、保留 sourceLog 和 notice、失敗時改用 Claude Code 內建 WebSearch |
| 11 | Grok 改成選用 | 只有 `grok` 模式才要求 Grok 的 key |
| 12 | 設定檔支援新變數 | 設定檔遇到不認識的欄位會整個拒絕，新變數要補進欄位清單 |
| 13 | 名稱改成 `llm-search` | 執行檔名稱、MCP server 名稱 |
| 14 | 測試 | 移植 DeepSeek plugin 的測試情境（分流順序、備援、verify 合併、金鑰遮蔽、網址驗證），加上 Brave 解析、Grok 選用 |

## 6. 驗證

1. `cargo fmt --check`、`cargo clippy`、`cargo test` 全部通過。
2. `cargo build --release`，產生 `target/release/llm-search`。
3. 用 `claude mcp add` 註冊，Brave / Tavily key 由你自己填。
4. 在 Claude Code 裡實測：
   - `provider_status`：不耗額度，顯示 Brave、Tavily 已設定
   - `quick`、`deep`、`verify` 各搜一次，檢查 sourceLog、verify 的合併結果
   - 故意填錯 Brave key：`quick` 要改用 Tavily，並出現 `notice`
   - `web_fetch` 給單一網址、多個網址各一次，檢查 contentHash
   - `doctor` 顯示各來源狀態
   - 切到 grok 模式搜一次
   - 檢查所有輸出都沒有出現 key

## 7. 這次不做

- npm 發佈、Docker、GitHub Actions release 流程相關檔案：保留不動。
- 把你現有設定裡的 `grok-search-rs` 換成 `llm-search`（settings.json 的 hook、`hooks/scan-dedicated-mcps.sh`、search-first 的 `tool-chains.md`）：新 MCP 確認能用後另外處理。
- 不 commit，除非你要求。

## 8. 風險

- **額度**：`verify` 一次扣 Brave 和 Tavily 兩邊；`deep` 用 Tavily advanced（2 credits）。Brave 每月免費約 $20，Tavily 每月免費 1,000 credits。
- **brave 模式沒有 AI 寫的答案**（`deep` 例外，附 Tavily 的答案）：Claude 拿到結果清單自己整理，可以逐條查證。
- **和兩個原專案都分岔**：之後原作者的更新要手動挑著合併。

---

## 附錄：程式碼位置（給實作時對照）

| 項目 | 位置 |
|---|---|
| 搜尋來源共同介面 `SourceProvider` | `src/service.rs` 開頭 |
| 來源規格 `ProviderSpec`、預設順序 `CANONICAL_SOURCE_ORDER` | `src/service.rs` |
| `web_search` 主流程 | `src/service.rs` 的 `SearchService::web_search` |
| 備援邏輯 | `src/service.rs` 的 `fetch_raw_extra_sources`、`finalize_fallback` |
| `doctor` | `src/service.rs` 的 `SearchService::doctor` |
| Grok 是否必填 | `src/service.rs` 的 `build_providers` / `build_providers_with_grok` |
| 環境變數與設定檔 | `src/config.rs`（`ConfigFile` 有 `deny_unknown_fields`） |
| 工具定義與說明文字 | `src/mcp.rs` |
| 來源實作範例 | `src/providers/tavily.rs`、`src/providers/exa.rs` |
| 名稱 | `Cargo.toml`、`src/mcp.rs`（serverInfo）、`src/main.rs` |
| 既有測試 | `tests/` |
| DeepSeek plugin 參考實作 | `search-router.ts`（分流）、`search-providers.ts`（Brave/Tavily 呼叫）、`search-mcp-server.ts`（sourceLog、遮蔽、health_check） |
