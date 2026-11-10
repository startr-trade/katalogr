#!/bin/sh
# Refuse to release a commit that has not passed CI.
#
#   GITHUB_REPOSITORY=owner/repo GITHUB_SHA=<commit> GH_TOKEN=... sh scripts/release-ci-gate.sh
#
# Run by release.yml's first job, before anything is built or published. It asks GitHub for the
# `ci` workflow's run on the commit being released: success proceeds, a failure or no run at all
# refuses, and a run still in progress is waited for (KATALOGR_CI_WAIT_TRIES x
# KATALOGR_CI_WAIT_SECONDS, 45 minutes by default).
#
# WHY. A tag is only a name for a commit. Nothing stops one being pushed while CI is still running,
# or after CI has failed (v0.1.0 was tagged with a red CI), and release.yml then builds and
# publishes binaries from that tree. A release is therefore gated on the CI run that already
# exists for the commit, rather than on a re-run of part of it, so the answer cannot drift from
# what the repository's checks report.
#
# GITHUB_SHA is passed EXPLICITLY by the workflow rather than taken from the ambient environment:
# on a `workflow_dispatch` re-release the ambient GITHUB_SHA is the dispatched branch's head, not
# the tagged commit, and gating the wrong commit is worse than not gating at all.
#
# GH can name a stand-in for the `gh` command, which is how the waiting is exercised without
# calling GitHub.
set -eu
: "${GITHUB_REPOSITORY:?owner/repo}"
: "${GITHUB_SHA:?the commit being released}"
gh_cmd="${GH:-gh}"
tries="${KATALOGR_CI_WAIT_TRIES:-90}"
pause="${KATALOGR_CI_WAIT_SECONDS:-30}"

i=0
while :; do
    i=$((i + 1))
    run="$("$gh_cmd" api "repos/${GITHUB_REPOSITORY}/actions/workflows/ci.yml/runs?head_sha=${GITHUB_SHA}&per_page=1" \
        -q '.workflow_runs[0] | select(.) | [.status, (.conclusion // ""), .html_url] | @tsv')"
    if [ -z "$run" ]; then
        echo "::error::CI has not run on ${GITHUB_SHA}: push the commit and let CI pass before tagging it." >&2
        exit 1
    fi
    status="$(printf '%s' "$run" | cut -f1)"
    conclusion="$(printf '%s' "$run" | cut -f2)"
    url="$(printf '%s' "$run" | cut -f3)"
    if [ "$status" = completed ]; then
        if [ "$conclusion" = success ]; then
            echo "CI passed on ${GITHUB_SHA}: ${url}"
            exit 0
        fi
        echo "::error::CI ${conclusion} on ${GITHUB_SHA}: nothing is released from a commit that failed its own gates: ${url}" >&2
        exit 1
    fi
    if [ "$i" -ge "$tries" ]; then
        echo "::error::CI is still ${status} on ${GITHUB_SHA} after waiting; re-run this release once it has passed: ${url}" >&2
        exit 1
    fi
    echo "CI is ${status} on ${GITHUB_SHA}; waiting (${i}/${tries})..."
    sleep "$pause"
done
