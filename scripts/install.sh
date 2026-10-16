#!/bin/sh
# katalogr installer. Usage: install.sh [--version vX.Y.Z] [--dir PATH]
# Env: KATALOGR_VERSION, KATALOGR_INSTALL_DIR, KATALOGR_RELEASE_BASE (test hook; file://DIR reads local files).
# Exit codes: 0 ok, 1 failure, 2 usage or unsupported platform, 3 checksum mismatch.
set -eu

BASE_DEFAULT="https://github.com/startr-trade/katalogr/releases"
BASE="${KATALOGR_RELEASE_BASE:-$BASE_DEFAULT}"
VERSION="${KATALOGR_VERSION:-latest}"
DIR="${KATALOGR_INSTALL_DIR:-${HOME:-}/.local/bin}"

die() { code=$1; shift; echo "install.sh: $*" >&2; exit "$code"; }

while [ $# -gt 0 ]; do
  case $1 in
    --version) [ $# -ge 2 ] || die 2 "--version needs a value"; VERSION=$2; shift 2 ;;
    --dir) [ $# -ge 2 ] || die 2 "--dir needs a value"; DIR=$2; shift 2 ;;
    -h|--help) echo "usage: install.sh [--version vX.Y.Z] [--dir PATH]"; exit 0 ;;
    *) die 2 "unknown argument: $1" ;;
  esac
done
[ -n "$DIR" ] || die 2 "no install directory (set --dir or HOME)"

# 1. Detect the target.
os=$(uname -s)
arch=$(uname -m)
case "$os/$arch" in
  Linux/x86_64|Linux/amd64) TARGET=x86_64-unknown-linux-musl ;;
  Linux/aarch64|Linux/arm64) TARGET=aarch64-unknown-linux-musl ;;
  Darwin/x86_64) TARGET=x86_64-apple-darwin ;;
  Darwin/arm64) TARGET=aarch64-apple-darwin ;;
  *) die 2 "unsupported platform: $os $arch" ;;
esac

TMP=$(mktemp -d) || die 1 "cannot create a temporary directory"
trap 'rm -rf "$TMP"' EXIT

# fetch <asset> <dest>: URL for GitHub, plain copy for file://.
fetch() {
  asset=$1; dest=$2
  case $BASE in
    file://*) cp "${BASE#file://}/$asset" "$dest" || die 1 "cannot read $asset from $BASE" ;;
    *)
      if [ "$VERSION" = latest ]; then url="$BASE/latest/download/$asset"; else url="$BASE/download/$VERSION/$asset"; fi
      if command -v curl >/dev/null 2>&1; then
        curl --proto '=https' --tlsv1.2 -fsSL -o "$dest" "$url" || die 1 "download failed: $url"
      elif command -v wget >/dev/null 2>&1; then
        wget -q -O "$dest" "$url" || die 1 "download failed: $url"
      else
        die 1 "neither curl nor wget is available"
      fi ;;
  esac
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1
  elif command -v shasum >/dev/null 2>&1; then shasum -a 256 "$1" | cut -d ' ' -f 1
  else die 1 "neither sha256sum nor shasum is available"; fi
}

# 2. Resolve the asset name. SHA256SUMS comes first; for "latest" it names the tag.
fetch SHA256SUMS "$TMP/SHA256SUMS"
if [ "$VERSION" = latest ]; then
  matches=$(awk -v suf="-$TARGET.tar.gz" '{ f=$2; sub(/^\*/, "", f); if (length(f) >= length(suf) && substr(f, length(f)-length(suf)+1) == suf) print f }' "$TMP/SHA256SUMS")
  count=$(printf '%s\n' "$matches" | grep -c . || true)
  [ "$count" -eq 1 ] || die 2 "expected exactly one $TARGET archive in SHA256SUMS, found $count"
  ASSET=$matches
else
  ASSET="katalogr-$VERSION-$TARGET.tar.gz"
fi

expected=$(awk -v a="$ASSET" '{ f=$2; sub(/^\*/, "", f); if (f == a) { print $1; exit } }' "$TMP/SHA256SUMS")
[ -n "$expected" ] || die 2 "$ASSET is not listed in SHA256SUMS"

fetch "$ASSET" "$TMP/$ASSET"

# 3. Verify. Nothing is installed unless this passes.
actual=$(sha256_of "$TMP/$ASSET")
if [ "$actual" != "$expected" ]; then
  echo "install.sh: checksum mismatch for $ASSET" >&2
  echo "  expected: $expected" >&2
  echo "  actual:   $actual" >&2
  rm -rf "$TMP"
  exit 3
fi

# 4. Extract and install.
mkdir "$TMP/x"
tar -xzf "$TMP/$ASSET" -C "$TMP/x" || die 1 "cannot extract $ASSET"
[ -f "$TMP/x/katalogr" ] || die 1 "archive does not contain katalogr"
mkdir -p "$DIR" || die 1 "cannot create $DIR"
if command -v install >/dev/null 2>&1; then
  install -m 0755 "$TMP/x/katalogr" "$DIR/katalogr" || die 1 "cannot install into $DIR"
else
  cp "$TMP/x/katalogr" "$DIR/katalogr" || die 1 "cannot install into $DIR"
  chmod 0755 "$DIR/katalogr"
fi

echo "installed $DIR/katalogr: $("$DIR/katalogr" --version)"
case ":${PATH:-}:" in
  *":$DIR:"*) ;;
  *) echo "note: $DIR is not on your PATH; add: export PATH=\"$DIR:\$PATH\"" ;;
esac
