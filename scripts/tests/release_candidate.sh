#!/bin/sh
# Release-candidate checks for katalogr (FEAT-082), printed in cargo-like form.
#
# scripts/tests/release_candidate.sh [CHECK...]
# With no argument, runs every check. Prints `test release_candidate::<check> ... ok|FAILED`,
# as install_test.sh does. Exit 1 if any failed, 2 for a usage or environment error.
#
# Env: KATALOGR_DATA     board's project folder (scale_fixes_done, tier_m_report_exists,
#                        every_item_ready, shipped_items_in_operations)
#      KATALOGR_RC_DIST  folder of packed archives and SHA256SUMS (static_*, version_*, install_*)
#      KATALOGR_RC_INSTALL  directory holding the installed katalogr (smoke_on_real_repository)
#      KATALOGR_RC_REPO  repository to clone for the smoke test (path or URL)
#      KATALOGR_RC_NOTES release notes file (notes_state_limits)
#      KATALOGR_RC_TIER_S  tier S report file (tier_s_report_names_commit)
#      KATALOGR_RC_COMMIT  full commit the candidate was built from
#
# A check whose variable is unset prints FAILED naming it, and never counts as passed.
# Needs: git, cargo, tar, readelf, sha256sum or shasum. None ships in the binary.
set -u
HERE=$(cd "$(dirname "$0")" && pwd)
REPO=$(cd "$HERE/../.." && pwd)
VERSION=v0.1.0
SPRINT=sprint/SP-002
RELEASE=release/$VERSION
T_X86=x86_64-unknown-linux-musl
T_ARM=aarch64-unknown-linux-musl

ALL_CHECKS="scale_fixes_done tier_m_report_exists every_item_ready feat006_followup_covered
release_contains_sprint main_is_ancestor_of_release static_x86_64 static_aarch64
version_names_release_commit install_from_file_base smoke_on_real_repository
workspace_checks_pass tier_s_report_names_commit notes_state_limits no_push_in_script
shipped_items_in_operations"

TMP=$(mktemp -d)
trap 'rm -rf "$TMP"' EXIT
FAILED=0
USAGE_ERR=0

# why <text>: the reason for the current check's failure, shown on stderr.
why() { echo "  $*" >&2; }

# need <VAR>: fail the check naming the variable if it is unset or empty.
need() {
  eval "v=\${$1:-}"
  if [ -z "$v" ]; then why "needs $1 to be set"; return 1; fi
  return 0
}

sha256_of() {
  if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1" | cut -d ' ' -f 1
  else shasum -a 256 "$1" | cut -d ' ' -f 1; fi
}

first12() { printf '%s' "$1" | cut -c 1-12; }

# item_file <CODE>: the item's yaml under KATALOGR_DATA, or nothing.
item_file() {
  for f in "$KATALOGR_DATA"/features/*/"$1".yaml; do
    [ -f "$f" ] && { printf '%s\n' "$f"; return 0; }
  done
  return 1
}

item_stage() { basename "$(dirname "$1")"; }

# ---- checks -------------------------------------------------------------------------------

scale_fixes_done() {
  need KATALOGR_DATA || return 1
  rc=0
  for c in FEAT-093 FEAT-076 FEAT-080 FEAT-075 FEAT-068 FEAT-079; do
    f=$(item_file "$c") || { why "$c has no item file"; rc=1; continue; }
    case "$(item_stage "$f")" in
      Migration|Operations) ;;
      "No Action") [ "$c" = FEAT-079 ] || { why "$c is under No Action"; rc=1; } ;;
      *) why "$c is under $(item_stage "$f")"; rc=1 ;;
    esac
  done
  return $rc
}

tier_m_report_exists() {
  need KATALOGR_DATA || return 1
  found=0
  for f in "$KATALOGR_DATA"/docs/research/load-test-*.md; do
    [ -f "$f" ] || continue
    grep -q -i -E 'tier[ _-]*M([^A-Za-z]|$)' "$f" || continue
    if grep -q -E '^\|[[:space:]]*katalogr[[:space:]]*\|[[:space:]]*unknown' "$f"; then
      why "$f names tier M but has a katalogr | unknown line"
      continue
    fi
    found=1
  done
  [ "$found" -eq 1 ] || { why "no docs/research/load-test-*.md names tier M with a known katalogr version"; return 1; }
}

every_item_ready() {
  need KATALOGR_DATA || return 1
  rc=0
  for f in "$KATALOGR_DATA"/features/*/FEAT-*.yaml; do
    [ -f "$f" ] || continue
    grep -q -E '^release:[[:space:]]*"?'"$VERSION"'"?[[:space:]]*$' "$f" || continue
    code=$(basename "$f" .yaml)
    stage=$(item_stage "$f")
    case "$stage" in
      Migration|Operations) ;;
      *) if [ "$code" != FEAT-022 ]; then why "$code is under $stage"; rc=1; fi ;;
    esac
    grep -q -E '^[[:space:]]*-?[[:space:]]*id:[[:space:]]*docs-folded' "$f" || { why "$code lacks the docs-folded sign-off"; rc=1; }
  done
  return $rc
}

feat006_followup_covered() {
  rc=0
  for t in esm_js_specifier_resolves_ts commonjs_exports_recorded extends_any_name constructor_calls_recorded; do
    out=$(cd "$REPO" && cargo test --locked -p katalogr-ext-ts "$t" 2>&1)
    if ! printf '%s\n' "$out" | grep -q -E '^test result: ok\. 1 passed'; then
      why "ts::$t did not report 1 passed"
      rc=1
    fi
  done
  return $rc
}

release_contains_sprint() {
  git -C "$REPO" merge-base --is-ancestor "$SPRINT" "$RELEASE" \
    || { why "$SPRINT is not an ancestor of $RELEASE"; return 1; }
}

main_is_ancestor_of_release() {
  git -C "$REPO" merge-base --is-ancestor main "$RELEASE" \
    || { why "main is not an ancestor of $RELEASE"; return 1; }
}

# unpack <target> <dir>: extract the archive for a target; prints nothing, returns 1 on failure.
unpack() {
  a="$KATALOGR_RC_DIST/katalogr-$VERSION-$1.tar.gz"
  [ -f "$a" ] || { why "missing $a"; return 1; }
  mkdir -p "$2"
  tar -xzf "$a" -C "$2" || { why "cannot extract $a"; return 1; }
  [ -f "$2/katalogr" ] || { why "$a holds no katalogr"; return 1; }
}

# A static-pie musl binary has .dynamic but no interpreter and no needed libraries.
static_for() {
  need KATALOGR_RC_DIST || return 1
  command -v readelf >/dev/null 2>&1 || { why "readelf is missing"; return 1; }
  d="$TMP/static-$1"
  unpack "$1" "$d" || return 1
  if readelf -l "$d/katalogr" 2>/dev/null | grep -q 'INTERP'; then why "$1 has an INTERP header"; return 1; fi
  if readelf -d "$d/katalogr" 2>/dev/null | grep -q 'NEEDED'; then why "$1 has NEEDED entries"; return 1; fi
  readelf -h "$d/katalogr" >/dev/null 2>&1 || { why "readelf cannot read $1"; return 1; }
}

static_x86_64() { static_for "$T_X86"; }
static_aarch64() { static_for "$T_ARM"; }

version_names_release_commit() {
  need KATALOGR_RC_DIST || return 1
  need KATALOGR_RC_COMMIT || return 1
  d="$TMP/version"
  unpack "$T_X86" "$d" || return 1
  out=$("$d/katalogr" --version 2>&1) || { why "--version failed: $out"; return 1; }
  case "$out" in
    *"$(first12 "$KATALOGR_RC_COMMIT")"*) ;;
    *) why "--version '$out' lacks $(first12 "$KATALOGR_RC_COMMIT")"; return 1 ;;
  esac
}

install_from_file_base() {
  need KATALOGR_RC_DIST || return 1
  dir=${KATALOGR_RC_INSTALL:-$TMP/install}
  out=$(KATALOGR_RELEASE_BASE="file://$KATALOGR_RC_DIST" sh "$REPO/scripts/install.sh" --version "$VERSION" --dir "$dir" 2>&1) \
    || { why "install.sh failed: $out"; return 1; }
  [ -x "$dir/katalogr" ] || { why "no executable at $dir/katalogr"; return 1; }
  "$dir/katalogr" --version >/dev/null 2>&1 || { why "installed binary does not run"; return 1; }
}

# json_first_name <file>: the first "name" value in a JSON answer.
json_first_name() {
  sed -n 's/.*"name"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$1" | head -n 1
}

smoke_on_real_repository() {
  need KATALOGR_RC_INSTALL || return 1
  need KATALOGR_RC_REPO || return 1
  bin="$KATALOGR_RC_INSTALL/katalogr"
  [ -x "$bin" ] || { why "no executable at $bin"; return 1; }
  w="$TMP/smoke"
  mkdir -p "$w"
  git clone -q --depth 1 -- "$KATALOGR_RC_REPO" "$w/clone" 2>"$w/clone.err" \
    || { why "clone failed: $(cat "$w/clone.err")"; return 1; }
  run() { # <label> <args...>: run in the clone with the kindex outside it
    label=$1; shift
    (cd "$w/clone" && "$bin" --kindex "$w/kindex" --project smoke "$@") >"$w/$label.out" 2>"$w/$label.err" \
      || { why "$label exited non-zero: $(head -c 300 "$w/$label.err")"; return 1; }
  }
  run kindex kindex init "$w/kindex" || return 1
  run project project init smoke --repo "clone=$w/clone" || return 1
  run index index --full || return 1
  run map --json map || return 1
  sym=$(json_first_name "$w/map.out")
  [ -n "$sym" ] || { why "map --json lists no symbol"; return 1; }
  run find --json find "$sym" || return 1
  run refs refs "$sym" || return 1
  run callers callers "$sym" || return 1
  run explain explain "$sym" || return 1
  run map2 map || return 1
  run hubs hubs || return 1
  run search search "$sym" || return 1
  run coverage coverage || return 1
  run doctor --json doctor --skip-git --fail-on error || return 1
  if grep -q -E '"severity"[[:space:]]*:[[:space:]]*"error"' "$w/doctor.out"; then
    why "doctor reports a finding of error severity"; return 1
  fi
}

workspace_checks_pass() {
  rc=0
  step() { # <label> <command...>
    label=$1; shift
    (cd "$REPO" && "$@") >"$TMP/ws.out" 2>&1 || { why "$label failed: $(tail -n 5 "$TMP/ws.out" | tr '\n' ' ')"; rc=1; }
  }
  step fmt cargo fmt --all -- --check
  step clippy cargo clippy --workspace --all-targets --locked -- -D warnings
  step clippy-no-default cargo clippy -p katalogr --no-default-features --all-targets --locked -- -D warnings
  step test cargo test --workspace --locked
  step failpoints cargo test -p katalogr-engine --features failpoints --locked
  step eval cargo run --locked -p katalogr -- eval run --sets fixtures/eval --baseline fixtures/eval/baseline.json --gate
  return $rc
}

tier_s_report_names_commit() {
  need KATALOGR_RC_TIER_S || return 1
  need KATALOGR_RC_COMMIT || return 1
  [ -f "$KATALOGR_RC_TIER_S" ] || { why "no report at $KATALOGR_RC_TIER_S"; return 1; }
  c=$(first12 "$KATALOGR_RC_COMMIT")
  grep -E '^\|[[:space:]]*katalogr[[:space:]]*\|' "$KATALOGR_RC_TIER_S" | grep -q "$c" \
    || { why "report's katalogr row lacks $c"; return 1; }
  # The "Corpus | Loaded" table: every row's second column must be `no`.
  awk -F'|' '
    /^\|[[:space:]]*Corpus[[:space:]]*\|[[:space:]]*Loaded[[:space:]]*\|/ { t = 1; seen = 1; next }
    t && /^\|[[:space:]]*-/ { next }
    t && /^\|/ { v = $3; gsub(/[[:space:]]/, "", v); if (v != "no") { print "corpus " $2 " was loaded"; bad = 1 } ; next }
    t { t = 0 }
    END { if (!seen) { print "no Corpus | Loaded table"; exit 1 } exit bad }
  ' "$KATALOGR_RC_TIER_S" >"$TMP/loaded.out" || { why "$(cat "$TMP/loaded.out")"; return 1; }
}

notes_state_limits() {
  need KATALOGR_RC_NOTES || return 1
  need KATALOGR_RC_COMMIT || return 1
  [ -f "$KATALOGR_RC_NOTES" ] || { why "no notes at $KATALOGR_RC_NOTES"; return 1; }
  rc=0
  grep -q "$(first12 "$KATALOGR_RC_COMMIT")" "$KATALOGR_RC_NOTES" || { why "notes lack the commit"; rc=1; }
  for h in Commit Targets Install Measured Limits; do
    grep -q -E "^##[[:space:]]+$h[[:space:]]*\$" "$KATALOGR_RC_NOTES" || { why "notes lack the heading ## $h"; rc=1; }
  done
  return $rc
}

# The needle is built from pieces so this check does not match its own text.
no_push_in_script() {
  p="pu"
  if grep -n -E "git[^|;&]*[[:space:]]${p}sh([[:space:]]|\$)" "$HERE/release_candidate.sh" >"$TMP/push.out"; then
    why "the script holds a push command: $(head -n 1 "$TMP/push.out")"
    return 1
  fi
}

shipped_items_in_operations() {
  need KATALOGR_DATA || return 1
  rc=0
  for f in "$KATALOGR_DATA"/features/Migration/FEAT-*.yaml; do
    [ -f "$f" ] || continue
    if grep -q -E '^release:[[:space:]]*"?'"$VERSION"'"?[[:space:]]*$' "$f"; then
      why "$(basename "$f" .yaml) is still under Migration"; rc=1
    fi
  done
  return $rc
}

# ---- driver -------------------------------------------------------------------------------

if [ $# -eq 0 ]; then set -- $ALL_CHECKS; fi

for chk in "$@"; do
  case " $(echo $ALL_CHECKS) " in
    *" $chk "*) ;;
    *) echo "release_candidate.sh: unknown check '$chk'" >&2; USAGE_ERR=1 ;;
  esac
done
[ "$USAGE_ERR" -eq 0 ] || exit 2
for tool in git tar; do
  command -v "$tool" >/dev/null 2>&1 || { echo "release_candidate.sh: $tool is missing" >&2; exit 2; }
done

for chk in "$@"; do
  if "$chk"; then
    echo "test release_candidate::$chk ... ok"
  else
    echo "test release_candidate::$chk ... FAILED"
    FAILED=1
  fi
done

exit "$FAILED"
