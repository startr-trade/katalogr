#!/usr/bin/env bash
# Browser check of the embedded web view (FEAT-033). OUTSIDE THE REQUIRED GATE: it needs a
# headless browser, so it is not in ci.yml. Run it by hand once on the final code.
#
#   tools/web-smoke.sh            build the release binary, index a synthetic project, serve it
#                                 on port 0, load /#/graph and /#/graph?tab=explore in a headless
#                                 browser, and require an <svg> (Pele) and a <canvas> (Cytoscape)
#                                 in the dumped DOM. Prints the drawing time of each page.
#
#   tools/web-smoke.sh --file PATH
#                                 open an exported file (`katalogr export html --out PATH`) by
#                                 file:// in a headless browser, with no server, and require a
#                                 diagram's <svg> in the dumped DOM. FEAT-034: this too stays
#                                 outside the required gate, and needs a browser on PATH.
#
# Exit codes: 0 pass; 1 a page lacks its <svg> or <canvas>, or the server did not start;
#             77 no browser found (chromium, chromium-browser or google-chrome).
# Set KATALOGR_BIN to use a prebuilt binary instead of building one.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"

browser=""
for b in chromium chromium-browser google-chrome; do
  if command -v "$b" >/dev/null 2>&1; then browser="$b"; break; fi
done
if [ -z "$browser" ]; then
  echo "web-smoke: no browser (chromium, chromium-browser or google-chrome) on PATH" >&2
  exit 77
fi

# A snap-packaged browser cannot write a profile under /tmp; give it one under its own folder.
smoke_profile() { # <fallback dir>
  if [ "$(readlink -f "$(command -v "$browser")")" = "/usr/bin/snap" ]; then
    mkdir -p "$HOME/snap/chromium/common/katalogr-web-smoke"
    echo "$HOME/snap/chromium/common/katalogr-web-smoke"
  else
    echo "$1"
  fi
}

if [ "${1:-}" = "--file" ]; then
  file="${2:-}"
  if [ -z "$file" ] || [ ! -f "$file" ]; then
    echo "web-smoke: --file wants an exported file that exists" >&2
    exit 2
  fi
  file="$(cd "$(dirname "$file")" && pwd)/$(basename "$file")"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/katalogr-web-smoke.XXXXXX")"
  trap 'rm -rf "$tmp"' EXIT
  dom="$("$browser" --headless=new --no-sandbox --disable-gpu --user-data-dir="$(smoke_profile "$tmp/profile")" \
    --dump-dom --virtual-time-budget=5000 "file://$file#/graph" 2>/dev/null || true)"
  if grep -q '<svg' <<<"$dom"; then
    echo "web-smoke: ok   $file <svg found (opened from disk, no server)"
    exit 0
  fi
  echo "web-smoke: FAIL $file no <svg in the DOM" >&2
  exit 1
fi

bin="${KATALOGR_BIN:-}"
if [ -z "$bin" ]; then
  (cd "$root" && cargo build --release -j4 -p katalogr)
  bin="$root/target/release/katalogr"
fi

work="$(mktemp -d "${TMPDIR:-/tmp}/katalogr-web-smoke.XXXXXX")"
server_pid=""
cleanup() {
  if [ -n "$server_pid" ]; then kill "$server_pid" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

# A synthetic project: a git repo holding a two-function Rust file, and a kindex for it.
repo="$work/repo"
mkdir -p "$repo/src"
cat > "$repo/src/lib.rs" <<'RS'
pub fn caller() -> u32 {
    callee() + 1
}

pub fn callee() -> u32 {
    1
}
RS
git -C "$repo" init -q
git -C "$repo" add -A
git -C "$repo" -c user.name=smoke -c user.email=smoke@example.invalid commit -q -m "synthetic"

kindex="$work/kindex"
"$bin" kindex init "$kindex" >/dev/null
"$bin" project init demo --kindex "$kindex" --repo "repo=$repo" --mark >/dev/null
(cd "$repo" && "$bin" index --full >/dev/null)

# Serve on port 0 and read the address the server announces.
log="$work/serve.log"
# exec, so $! is the server itself and the EXIT trap stops it (a plain subshell left it
# running, holding any lock fd the caller passed in).
(cd "$repo" && exec "$bin" serve --listen 127.0.0.1:0 2>"$log" >/dev/null) &
server_pid=$!
addr=""
for _ in $(seq 1 100); do
  addr="$(sed -n 's/.*listening on http:\/\/\([0-9.:]*\).*/\1/p' "$log" | head -n1)"
  [ -n "$addr" ] && break
  if ! kill -0 "$server_pid" 2>/dev/null; then break; fi
  sleep 0.1
done
if [ -z "$addr" ]; then
  echo "web-smoke: the server did not announce an address:" >&2
  cat "$log" >&2
  exit 1
fi

# A snap-packaged browser cannot write a profile under /tmp; give it one under its own folder.
profile="$work/profile"
if [ "$(readlink -f "$(command -v "$browser")")" = "/usr/bin/snap" ]; then
  profile="$HOME/snap/chromium/common/katalogr-web-smoke"
  mkdir -p "$profile"
fi

status=0
check() { # <path> <needle>
  local url="http://$addr/$1" start end dom
  start="$(date +%s.%N)"
  dom="$("$browser" --headless=new --no-sandbox --disable-gpu --user-data-dir="$profile" \
    --dump-dom --virtual-time-budget=5000 "$url" 2>/dev/null || true)"
  end="$(date +%s.%N)"
  if grep -q "$2" <<<"$dom"; then
    printf 'web-smoke: ok   %-28s %s found, %.2f s\n' "$1" "$2" "$(echo "$end - $start" | bc -l)"
  else
    printf 'web-smoke: FAIL %-28s no %s in the DOM\n' "$1" "$2" >&2
    status=1
  fi
}
check '#/graph' '<svg'
check '#/graph?tab=explore' '<canvas'
exit "$status"
