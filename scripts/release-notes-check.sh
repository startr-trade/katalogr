#!/bin/sh
# A version is released only with its notes (FEAT-114): docs/src/releases/<tag>.md exists, is not
# empty, and is linked from the release notes index and from the book's table of contents.
# Usage: release-notes-check.sh <tag, e.g. v0.2.0>
set -eu
tag="$1"
notes="docs/src/releases/${tag}.md"
fail=0
if [ ! -s "$notes" ]; then
  echo "::error::no release notes for ${tag}: write ${notes}" >&2
  fail=1
fi
if ! grep -qF "(${tag}.md)" docs/src/releases/index.md; then
  echo "::error::docs/src/releases/index.md does not list ${tag}" >&2
  fail=1
fi
if ! grep -qF "(releases/${tag}.md)" docs/src/SUMMARY.md; then
  echo "::error::docs/src/SUMMARY.md does not list releases/${tag}.md" >&2
  fail=1
fi
exit "$fail"
