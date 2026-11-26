#!/bin/bash
# Usage: prepare-busy.sh <line>
#
# Fails when a release of the line X.Y (for example 1.3) is under way, so
# release-X.Y must not move now: a draft release vX.Y.* is waiting, a branch
# pinned/vX.Y.* exists, or a release gate run is queued or going.
#
# release-prepare.yml runs it in the check job, and again in the push job
# right before it pushes, because the approval in between can take hours.
# GitHub shows draft releases only to a token with write access, and lists
# runs only with actions: read; the push job's token has both.
#
# Prints nothing on success. Needs gh, logged in (GH_TOKEN), and GH_REPO.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <line>" >&2; exit 2; }
line=$1
branch=release-$line

fail() {
    echo "::error::$1" >&2
    exit 1
}

[[ $line =~ ^[0-9]+\.[0-9]+$ ]] || fail "'$line' is not a release line such as 1.3."

# Each answer goes into a variable first, then grep searches it (see
# release-start-check.sh). A failed gh call stops the script.
gh_lines() {
    gh api --paginate "$1" --jq "$2" ||
        fail "Could not read $1 from GitHub."
}

drafts=$(gh_lines "repos/$GH_REPO/releases" '.[] | select(.draft) | .tag_name')
draft=$(grep -E -m1 "^v${line//./\\.}\\." <<<"$drafts" || true)
if [ -n "$draft" ]; then
    fail "The draft release $draft is waiting. Publish it (.github/scripts/sign-release.sh --publish ${draft#v}) or delete it (gh release delete $draft) before $branch moves."
fi

pins=$(gh_lines "repos/$GH_REPO/git/matching-refs/heads/pinned/v$line." '.[].ref | ltrimstr("refs/heads/")')
pin=$(grep -E -m1 "^pinned/v${line//./\\.}\\." <<<"$pins" || true)
if [ -n "$pin" ]; then
    fail "The branch $pin exists: a release of the line is under way. Let it finish (or remove the branch if its run is over) before $branch moves."
fi

statuses=$(gh run list --workflow release-gate.yml --limit 100 --json status --jq '.[].status') ||
    fail "Could not list the release-gate.yml runs from GitHub."
if grep -qxE 'queued|in_progress|waiting|pending|requested' <<<"$statuses"; then
    fail "A release gate run is going. Wait for it to finish, then start this workflow again."
fi
