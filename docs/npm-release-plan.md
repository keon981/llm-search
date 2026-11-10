# llm-search npm 發佈計畫

目標：其他電腦不用 fork、不用裝 Rust，用 `vp` 全域安裝 llm-search 後直接註冊 MCP。

## 做法

沿用上游已有的發版流程：推一個版本 tag（例如 `v0.1.0`）→ GitHub Actions 編 5 個平台的執行檔 → 建 GitHub Release → 發佈 npm 套件。只把名稱從 grok-search-rs 換成 llm-search。

npm 套件結構（與上游相同）：

| 套件 | 內容 |
|---|---|
| `@<npm帳號>/llm-search` | 主套件，只有一支 `run.js`，依目前平台找出對應的執行檔再啟動 |
| `@<npm帳號>/llm-search-darwin-universal` 等 5 個 | 各平台編好的執行檔（macOS universal、Linux x64/arm64、Windows x64/arm64） |

npm 上 `llm-search` 已被其他人佔用，所以主套件也要放在你的帳號底下。

## 要改的檔案

| 檔案 | 改動 |
|---|---|
| `.github/workflows/release.yml` | 產出檔名、執行檔名、`cargo update -p` 改成 llm-search；npm 發佈路徑改成 `npm/llm-search` |
| `.github/workflows/bump.yml`、`ci.yml` | 同上，名稱替換 |
| `npm/grok-search-rs/` → `npm/llm-search/` | `package.json` 的名稱、`bin`（指令名 `llm-search`）、repository、author；`run.js` 的套件名對照表 |
| `npm/platforms/*/package.json` | 5 個平台套件改名、`files` 改成 `bin/llm-search` |
| `Cargo.toml` | `repository`、`homepage` 改成你的 GitHub repo |
| `README.md` | 安裝段改成 `vp` 全域安裝 + `claude mcp add` |
| `RELEASING.md` | 改成 llm-search 版本，含 `NPM_TOKEN` 設定步驟 |

## 待決定

1. Docker 映像檔與 Linux HTTP 伺服器版：建議拿掉（release.yml 的 docker job 與 `-http` 兩個建置），只編本機用的版本。
2. 第一個版本號：建議 `v0.1.0`（版本由 tag 決定，CI 會自動同步回 `Cargo.toml` 與所有 `package.json`）。

## 推上 GitHub 與發版（你要做的部分）

```bash
gh auth login                                   # 這台電腦尚未登入
cd ~/Documents/llm/llm-search
gh repo create <repo名稱> --public --source . --push
gh secret set NPM_TOKEN                          # 貼上 npm 的 Automation / Granular token，輸入不顯示
git tag v0.1.0 && git push origin v0.1.0         # 觸發 release.yml
```

## 其他電腦安裝

```bash
vp install -g @<npm帳號>/llm-search
claude mcp add llm-search -s user -e BRAVE_API_KEY=YOUR_BRAVE_KEY -e TAVILY_API_KEY=YOUR_TAVILY_KEY -- llm-search
```

（`vp` 全域安裝的實際指令在實作時以 `vp --help` 確認。）

## 驗證

- 本機：`cargo test`、`cargo clippy`；`node npm/llm-search/run.js --version` 在找不到平台套件時給出正確錯誤訊息。
- workflow：`actionlint`（若可用）或人工檢查所有 `grok-search-rs` 字樣已替換。
- 發版後：GitHub Release 有 5 個平台壓縮檔與 `SHA256SUMS`；在另一台電腦 `vp` 安裝後 `llm-search --version` 正常、`provider_status` 可呼叫。

## 風險

- npm 帳號若開了 2FA，`NPM_TOKEN` 要用 Automation 或 Granular token，否則 CI 發佈會失敗。
- CI 會把版本號 commit 回 `main`，repo 的 `main` 不能設會擋住 `github-actions[bot]` 的保護規則。
- 第一次發版前無法在本機完整驗證 workflow，失敗時要看 Actions log 修正後重推 tag。
