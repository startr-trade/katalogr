#!/bin/sh
# Shell tests for scripts/install.sh, printed in cargo-like form.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
INSTALL="$HERE/../install.sh"

case "$(uname -s)/$(uname -m)" in
  Linux/x86_64|Linux/amd64) TARGET=x86_64-unknown-linux-musl ;;
  Linux/aarch64|Linux/arm64) TARGET=aarch64-unknown-linux-musl ;;
  Darwin/x86_64) TARGET=x86_64-apple-darwin ;;
  Darwin/arm64) TARGET=aarch64-apple-darwin ;;
  *) echo "install_test.sh: unsupported platform" >&2; exit 2 ;;
esac

ROOT=$(mktemp -d)
trap 'rm -rf "$ROOT"' EXIT
FAILED=0

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1
  else shasum -a 256 "$1" | cut -d ' ' -f 1; fi
}

# make_release <name>: builds $ROOT/<name>/rel with a good archive and SHA256SUMS.
make_release() {
  d="$ROOT/$1"
  mkdir -p "$d/pkg" "$d/rel"
  printf '#!/bin/sh\necho "katalogr 0.0.0 (test)"\n' > "$d/pkg/katalogr"
  chmod 0755 "$d/pkg/katalogr"
  asset="katalogr-v0.0.0-$TARGET.tar.gz"
  tar -czf "$d/rel/$asset" -C "$d/pkg" katalogr
  printf '%s  %s\n' "$(sha256_of "$d/rel/$asset")" "$asset" > "$d/rel/SHA256SUMS"
}

report() { # <name> <0|1>
  if [ "$2" -eq 0 ]; then echo "test install::$1 ... ok"; else echo "test install::$1 ... FAILED"; FAILED=1; fi
}

# checksum_verified (R-2)
make_release ok
rc=0
KATALOGR_RELEASE_BASE="file://$ROOT/ok/rel" KATALOGR_INSTALL_DIR="$ROOT/ok/bin" \
  sh "$INSTALL" >"$ROOT/ok/out" 2>&1 || rc=$?
r=1
if [ "$rc" -eq 0 ] && [ -x "$ROOT/ok/bin/katalogr" ] \
  && [ "$("$ROOT/ok/bin/katalogr" --version)" = "katalogr 0.0.0 (test)" ]; then r=0; else cat "$ROOT/ok/out" >&2; fi
report checksum_verified "$r"

# mismatch_refused (R-3): corrupt the archive after the sums were computed.
make_release bad
printf 'corrupt' >> "$ROOT/bad/rel/katalogr-v0.0.0-$TARGET.tar.gz"
rc=0
KATALOGR_RELEASE_BASE="file://$ROOT/bad/rel" KATALOGR_INSTALL_DIR="$ROOT/bad/bin" \
  sh "$INSTALL" >"$ROOT/bad/out" 2>&1 || rc=$?
r=1
if [ "$rc" -eq 3 ] && [ ! -e "$ROOT/bad/bin/katalogr" ] && grep -q expected "$ROOT/bad/out" && grep -q actual "$ROOT/bad/out"; then r=0; else cat "$ROOT/bad/out" >&2; fi
report mismatch_refused "$r"

exit "$FAILED"
