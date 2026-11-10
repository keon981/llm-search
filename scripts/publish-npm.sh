#!/usr/bin/env bash
# Publish @keon981/llm-search to npm from this machine with `vp pm publish`.
#
# The binaries come from the GitHub Release that release.yml builds for the
# tag, so no Rust toolchain or cross-compilation is needed here.
#
#   scripts/publish-npm.sh 0.2.0            # publish
#   scripts/publish-npm.sh 0.2.0 --dry-run  # pack and preview only
#
# Requires: curl, tar, unzip, node, vp, and `vp pm login` done beforehand.
set -euo pipefail

VERSION="${1:?usage: scripts/publish-npm.sh <version> [--dry-run]}"
VERSION="${VERSION#v}"
DRY_RUN="${2:-}"
REPO="keon981/llm-search"
BASE="https://github.com/${REPO}/releases/download/v${VERSION}"

cd "$(dirname "$0")/.."

# platform dir | release asset | binary name
PLATFORMS=(
  "linux-x64|llm-search_Linux_x86_64.tar.gz|llm-search"
  "linux-arm64|llm-search_Linux_aarch64.tar.gz|llm-search"
  "darwin-universal|llm-search_Darwin_universal.tar.gz|llm-search"
  "win32-x64|llm-search_Windows_x86_64.zip|llm-search.exe"
  "win32-arm64|llm-search_Windows_aarch64.zip|llm-search.exe"
)

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK" npm/platforms/*/bin' EXIT

echo "Downloading release v${VERSION} assets and checksums"
curl -fsSL "${BASE}/SHA256SUMS" -o "$WORK/SHA256SUMS"
for entry in "${PLATFORMS[@]}"; do
  IFS='|' read -r _ asset _ <<<"$entry"
  curl -fsSL "${BASE}/${asset}" -o "$WORK/${asset}"
done
(cd "$WORK" && shasum -a 256 -c SHA256SUMS)

echo "Setting version ${VERSION} in all package.json files"
VERSION="$VERSION" node - <<'NODE'
const fs = require('fs');
const path = require('path');
const version = process.env.VERSION;
const write = (p, pkg) => fs.writeFileSync(p, JSON.stringify(pkg, null, 2) + '\n');
const mainPath = 'npm/llm-search/package.json';
const main = JSON.parse(fs.readFileSync(mainPath, 'utf8'));
main.version = version;
for (const dep of Object.keys(main.optionalDependencies || {})) main.optionalDependencies[dep] = version;
write(mainPath, main);
for (const entry of fs.readdirSync('npm/platforms')) {
  const p = path.join('npm/platforms', entry, 'package.json');
  if (!fs.existsSync(p)) continue;
  const pkg = JSON.parse(fs.readFileSync(p, 'utf8'));
  pkg.version = version;
  write(p, pkg);
}
NODE

PUBLISH_ARGS=(--access public --no-git-checks)
[ "$DRY_RUN" = "--dry-run" ] && PUBLISH_ARGS+=(--dry-run)

# npm accounts with 2FA need a one-time password on every publish. A code
# lives ~30s, so with a terminal attached the script asks before each
# package; NPM_OTP (one code for all) works only if the run is that quick.
publish() {
  local dir="$1" otp="${NPM_OTP:-}"
  if [ -z "$otp" ] && [ "$DRY_RUN" != "--dry-run" ] && [ -t 0 ]; then
    read -r -p "OTP for ${dir} (Enter to skip): " otp
  fi
  if [ -n "$otp" ]; then
    vp pm publish "$dir" "${PUBLISH_ARGS[@]}" --otp "$otp"
  else
    vp pm publish "$dir" "${PUBLISH_ARGS[@]}"
  fi
}

# Platform packages first: the main package's optionalDependencies point at them.
for entry in "${PLATFORMS[@]}"; do
  IFS='|' read -r platform asset binary <<<"$entry"
  dir="npm/platforms/${platform}"
  rm -rf "${dir}/bin" && mkdir -p "${dir}/bin"
  if [[ "$asset" == *.zip ]]; then
    unzip -q "$WORK/${asset}" -d "${dir}/bin"
  else
    tar -xzf "$WORK/${asset}" -C "${dir}/bin"
  fi
  [[ "$binary" == *.exe ]] || chmod +x "${dir}/bin/${binary}"
  if [ "$DRY_RUN" != "--dry-run" ] && vp pm view "@keon981/llm-search-${platform}@${VERSION}" version >/dev/null 2>&1; then
    echo "Skip ${platform}: ${VERSION} already published"
    continue
  fi
  echo "Publishing ${platform}"
  publish "$dir"
done

echo "Publishing main package"
publish npm/llm-search

# Keep the extracted binaries out of git.
for entry in "${PLATFORMS[@]}"; do
  IFS='|' read -r platform _ _ <<<"$entry"
  rm -rf "npm/platforms/${platform}/bin"
done
echo "Done: @keon981/llm-search@${VERSION}"
