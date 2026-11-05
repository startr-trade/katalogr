#!/usr/bin/env bash
# Vendor Pele's browser build into katalogr-render (FEAT-032 R-9).
#
#   tools/vendor-pele.sh           fetch pele-<version>.tgz from the npm registry, verify its sha512
#                                  integrity, and write dist/pele.global.js, LICENSE and SHA256SUMS
#   tools/vendor-pele.sh --check   fetch and verify the same way, then compare with the committed
#                                  files; exit 1 on any difference, 2 on a fetch or integrity failure
#
# Needs network access, curl, tar, openssl, sha256sum and cmp. Nothing else (no npm).
# The version is pinned exactly and must match the one kanbanr uses (kanbanr FEAT-203); updating
# it is a deliberate change in both tools, with the new integrity value below.
set -euo pipefail

VERSION="1.7.0"
INTEGRITY="sha512-nkYb5VnxsTgwD4NThF6j4pJEC4AhGLJ9ptiL4q/vIzGr9q6ZtoRE3hFr94LgHpZ/xy56d1pw42UAQGaZkjM5XA=="
URL="https://registry.npmjs.org/pele/-/pele-${VERSION}.tgz"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/crates/katalogr-render/assets/pele"
mode="write"
case "${1:-}" in
  "") ;;
  --check) mode="check" ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl --fail --silent --show-error --location --output "$work/pele.tgz" "$URL" \
  || { echo "vendor-pele: could not fetch $URL" >&2; exit 2; }

actual="sha512-$(openssl dgst -sha512 -binary "$work/pele.tgz" | base64 -w0)"
if [ "$actual" != "$INTEGRITY" ]; then
  echo "vendor-pele: integrity mismatch for pele-${VERSION}.tgz" >&2
  echo "  expected $INTEGRITY" >&2
  echo "  actual   $actual" >&2
  exit 2
fi

mkdir "$work/pkg"
tar -xzf "$work/pele.tgz" -C "$work/pkg" package/dist/pele.global.js package/LICENSE
src="$work/pkg/package"
(cd "$src/dist" && sha256sum pele.global.js > "$work/SHA256SUMS")
(cd "$src" && sha256sum LICENSE >> "$work/SHA256SUMS")

if [ "$mode" = "write" ]; then
  mkdir -p "$dest"
  cp "$src/dist/pele.global.js" "$dest/pele.global.js"
  cp "$src/LICENSE" "$dest/LICENSE"
  cp "$work/SHA256SUMS" "$dest/SHA256SUMS"
  echo "vendor-pele: wrote pele ${VERSION} to ${dest#"$root"/}"
  exit 0
fi

status=0
for pair in "dist/pele.global.js:pele.global.js" "LICENSE:LICENSE"; do
  from="${pair%%:*}"; to="${pair##*:}"
  if ! cmp -s "$src/$from" "$dest/$to"; then
    echo "vendor-pele: $to differs from the pinned package" >&2
    status=1
  fi
done
if ! cmp -s "$work/SHA256SUMS" "$dest/SHA256SUMS"; then
  echo "vendor-pele: SHA256SUMS differs from the pinned package" >&2
  status=1
fi
if [ "$status" -eq 0 ]; then
  echo "vendor-pele: vendored copy is byte-identical to pele ${VERSION}"
fi
exit "$status"
