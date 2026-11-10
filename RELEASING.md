# 發版

分兩段：GitHub Actions 編執行檔並建立 Release，再從本機用 `vp pm publish` 發佈 npm。

## 一次性設定

- `vp pm login`，`vp pm whoami` 要顯示 `keon981`。
- `main` 不要設會擋住 `github-actions[bot]` push 的保護規則（發版後 CI 會把版本號 commit 回 `main`）。

## 每次發版

```bash
# 1. 推 tag，觸發 .github/workflows/release.yml
git tag v0.2.0
git push origin v0.2.0

# 2. 等 Release 建好（Actions 頁面 Release 流程完成）

# 3. 從本機發佈 npm（先 --dry-run 預覽）
scripts/publish-npm.sh 0.2.0 --dry-run
scripts/publish-npm.sh 0.2.0
```

`release.yml` 會：

1. 把 tag 的版本號寫進 `Cargo.toml`，編 5 個平台的執行檔（Linux x64 / arm64 musl、macOS universal、Windows x64 / arm64）。
2. 建立 GitHub Release，附上壓縮檔和 `SHA256SUMS`。
3. 把版本號同步回 `main` 的 `Cargo.toml`、`Cargo.lock` 和所有 `package.json`（commit 名為 `chore: sync manifests to X.Y.Z`）。

`scripts/publish-npm.sh` 會：

1. 從該版 GitHub Release 下載 5 個壓縮檔和 `SHA256SUMS`，比對 checksum。
2. 把所有 `package.json` 的版本設成該版。
3. 解開執行檔放進 `npm/platforms/<平台>/bin`，依序發佈 5 個平台套件 `@keon981/llm-search-<平台>`，最後發佈主套件 `@keon981/llm-search`。
4. 清掉解開的執行檔。

npm 帳號開了 2FA 時，`vp pm publish` 會要求輸入 OTP。

## 發版後檢查

- GitHub Release 頁面有 5 個壓縮檔和 `SHA256SUMS`。
- `vp install -g @keon981/llm-search@X.Y.Z` 後 `llm-search --version` 顯示新版本。

> 本機保留了上游 GrokSearch-rs 的 tag（v0.1.0～v0.1.26），不要推到 GitHub；新版本從 v0.2.0 起算，避開這些名稱。
