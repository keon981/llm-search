# 發版

## 一次性設定

1. 在 npm 建立 **Granular Access Token**（或 Automation token），權限要能發佈 `@k` 底下的套件。帳號開了 2FA 時，一般 token 在 CI 會失敗。
2. 存進 GitHub repo secret：

   ```bash
   gh secret set NPM_TOKEN --repo keon981/llm-search
   ```

3. `main` 不要設會擋住 `github-actions[bot]` push 的保護規則（發版後 CI 會把版本號 commit 回 `main`）。

## 每次發版

```bash
git tag v0.1.0
git push origin v0.1.0
```

推 tag 會觸發 `.github/workflows/release.yml`：

1. 把 tag 的版本號寫進 `Cargo.toml`，編 5 個平台的執行檔（Linux x64 / arm64 musl、macOS universal、Windows x64 / arm64）。
2. 建立 GitHub Release，附上壓縮檔和 `SHA256SUMS`。
3. 發佈 5 個平台套件 `@k/llm-search-<平台>`，再發佈主套件 `@k/llm-search`。
4. 把版本號同步回 `main` 的 `Cargo.toml`、`Cargo.lock` 和所有 `package.json`（commit 名為 `chore: sync manifests to X.Y.Z`）。

可以先在 CHANGELOG.md 加一段再打 tag。

## 發版後檢查

- GitHub Release 頁面有 5 個壓縮檔和 `SHA256SUMS`。
- `vp install -g @k/llm-search@X.Y.Z` 後 `llm-search --version` 顯示新版本。
- `main` 出現 `chore: sync manifests to X.Y.Z`。

## 手動備案

CI 不能用時，`scripts/bump-version.sh X.Y.Z --push` 會在本機改版本號、commit、打 tag、push。
