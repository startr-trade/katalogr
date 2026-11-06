#!/usr/bin/env bash
# Vendor Cytoscape.js's browser build into katalogr-web (FEAT-033 R-7).
#
#   tools/vendor-cytoscape.sh           fetch cytoscape-<version>.tgz from the npm registry, verify its sha512
#                                       integrity, and write cytoscape.min.js, LICENSE and SHA256SUMS
#   tools/vendor-cytoscape.sh --check   fetch and verify the same way, then compare with the committed
#                                       files; exit 1 on any difference, 2 on a fetch or integrity failure
#
# Needs network access, curl, tar, openssl, sha256sum and cmp. Nothing else (no npm).
# The version is pinned exactly. Updating it is a deliberate change here, with the new integrity value below.
set -euo pipefail

VERSION="3.34.4"
INTEGRITY="sha512-msY99tMY8iWTg+sIPGfSqDcPNOt4xcj+v/K/7qUFaCUxxlqnMkUYLBjRUzn9W5CU/R2hrmtJa5+UtovrEM2ctg=="
URL="https://registry.npmjs.org/cytoscape/-/cytoscape-${VERSION}.tgz"

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="$root/crates/katalogr-web/assets/cytoscape"
mode="write"
case "${1:-}" in
  "") ;;
  --check) mode="check" ;;
  *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

curl --fail --silent --show-error --location --output "$work/cytoscape.tgz" "$URL" \
  || { echo "vendor-cytoscape: could not fetch $URL" >&2; exit 2; }

actual="sha512-$(openssl dgst -sha512 -binary "$work/cytoscape.tgz" | base64 -w0)"
if [ "$actual" != "$INTEGRITY" ]; then
  echo "vendor-cytoscape: integrity mismatch for cytoscape-${VERSION}.tgz" >&2
  echo "  expected $INTEGRITY" >&2
  echo "  actual   $actual" >&2
  exit 2
fi

mkdir "$work/pkg"
tar -xzf "$work/cytoscape.tgz" -C "$work/pkg" package/dist/cytoscape.min.js package/LICENSE
src="$work/pkg/package"
(cd "$src/dist" && sha256sum cytoscape.min.js > "$work/SHA256SUMS")
(cd "$src" && sha256sum LICENSE >> "$work/SHA256SUMS")

if [ "$mode" = "write" ]; then
  mkdir -p "$dest"
  cp "$src/dist/cytoscape.min.js" "$dest/cytoscape.min.js"
  cp "$src/LICENSE" "$dest/LICENSE"
  cp "$work/SHA256SUMS" "$dest/SHA256SUMS"
  echo "vendor-cytoscape: wrote cytoscape ${VERSION} to ${dest#"$root"/}"
  exit 0
fi

status=0
for pair in "dist/cytoscape.min.js:cytoscape.min.js" "LICENSE:LICENSE"; do
  from="${pair%%:*}"; to="${pair##*:}"
  if ! cmp -s "$src/$from" "$dest/$to"; then
    echo "vendor-cytoscape: $to differs from the pinned package" >&2
    status=1
  fi
done
if ! cmp -s "$work/SHA256SUMS" "$dest/SHA256SUMS"; then
  echo "vendor-cytoscape: SHA256SUMS differs from the pinned package" >&2
  status=1
fi
if [ "$status" -eq 0 ]; then
  echo "vendor-cytoscape: vendored copy is byte-identical to cytoscape ${VERSION}"
fi
exit "$status"
