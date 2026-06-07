#!/usr/bin/env bash
# Collect + normalize the packaged artifacts and generate the
# cargo-packager-updater manifest (latest.json) + SHA256SUMS. Run after
# `cargo packager` in CI (or locally) and upload everything in <out-dir>.
#
# Usage: scripts/make-manifest.sh <tag> <artifact-dir> [out-dir]
#   <tag>          release tag, e.g. v0.1.0 (leading "v" optional)
#   <artifact-dir> dir containing the packaged *.app.tar.gz/.sig/.dmg
#   [out-dir]      upload staging dir (default: dist/)
#
# Why normalize: cargo-packager emits names with a space ("LEMON record.app.tar.gz")
# and no version/arch. GitHub Releases rewrites spaces in asset names, which would
# break the manifest URL — so we rename to space-free, versioned, arch-tagged
# names here and point latest.json at those. The minisign signature is over the
# tarball BYTES, not its name, so renaming the tarball + carrying its .sig is safe.
#
# The updater downloads the signed .app.tar.gz; the .dmg is the human download.
# latest.json is itself served at the stable .../releases/latest/download/latest.json
# URL the app polls.
set -euo pipefail

REPO="${LEMON_REPO:-chronick/lemon-record}"
TAG="${1:?usage: make-manifest.sh <tag> <artifact-dir> [out-dir]}"
ART_DIR="${2:?missing artifact dir}"
OUT_DIR="${3:-dist}"
VERSION="${TAG#v}"

command -v jq >/dev/null || { echo "jq is required" >&2; exit 1; }
rm -rf "$OUT_DIR"; mkdir -p "$OUT_DIR"

case "$(uname -m)" in
  arm64|aarch64) ARCH="aarch64" ;;
  x86_64)        ARCH="x86_64" ;;
  *) echo "unsupported arch $(uname -m)" >&2; exit 1 ;;
esac
SLUG="LEMON-record_${VERSION}_${ARCH}"

# Locate the source artifacts (names contain spaces).
src_tar="$(find "$ART_DIR" -maxdepth 2 -name '*.app.tar.gz' -print -quit)"
src_dmg="$(find "$ART_DIR" -maxdepth 2 -name '*.dmg' -print -quit)"
[ -n "$src_tar" ] || { echo "no *.app.tar.gz under $ART_DIR" >&2; exit 1; }
[ -f "${src_tar}.sig" ] || { echo "missing ${src_tar}.sig (was CARGO_PACKAGER_SIGN_PRIVATE_KEY set?)" >&2; exit 1; }

# Normalized upload set.
tar_name="${SLUG}.app.tar.gz"
cp "$src_tar"        "$OUT_DIR/${tar_name}"
cp "${src_tar}.sig"  "$OUT_DIR/${tar_name}.sig"
[ -n "$src_dmg" ] && cp "$src_dmg" "$OUT_DIR/${SLUG}.dmg"

url="https://github.com/${REPO}/releases/download/${TAG}/${tar_name}"
pub_date="$(date -u +%Y-%m-%dT%H:%M:%SZ)"

jq -n \
  --arg version "$VERSION" \
  --arg pub_date "$pub_date" \
  --arg plat "macos-${ARCH}" \
  --arg url "$url" \
  --rawfile signature "$OUT_DIR/${tar_name}.sig" \
  '{
     version: $version,
     pub_date: $pub_date,
     platforms: { ($plat): { signature: $signature, url: $url, format: "app" } }
   }' > "$OUT_DIR/latest.json"

( cd "$OUT_DIR" && shasum -a 256 *.app.tar.gz *.dmg 2>/dev/null > SHA256SUMS || true )

echo "=== $OUT_DIR ==="
ls -1 "$OUT_DIR"
echo "=== latest.json ==="
cat "$OUT_DIR/latest.json"
