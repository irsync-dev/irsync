#!/bin/bash
# Usage: pin-release.sh <tag> <branch>
#
# The release gate's pin step. It makes the branch pinned/<tag> point at
# GITHUB_SHA, the commit the gate's tests ran on; release.yml then runs on
# that branch, so it builds exactly that commit. <branch> is the branch the
# gate runs on (main or release-X.Y).
#
# Before pinning:
# - Stop if the tag exists now: an earlier attempt published the release
#   while the tests ran.
# - Delete a pinned/<tag> left over from an earlier attempt. The tag does
#   not exist, so that attempt released nothing; the gate's step before
#   this one made sure no run of it is still going.
#
# Needs gh, logged in (GH_TOKEN), GH_REPO and GITHUB_SHA.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <tag> <branch>" >&2; exit 2; }
tag=$1
branch=$2
pin=pinned/$tag

# Each answer goes into a variable first, then grep -q searches it (see
# release-start-check.sh). A failed gh call stops the script; it must not
# read as "not there".
gh_lines() {
    gh api "$1" --jq "$2" ||
        { echo "::error::Could not read $1 from GitHub." >&2; exit 1; }
}

tags=$(gh_lines "repos/$GH_REPO/git/matching-refs/tags/$tag" '.[].ref | ltrimstr("refs/tags/")')
if grep -qxF -- "$tag" <<<"$tags"; then
    echo "::error::The tag $tag exists now: an earlier attempt published the release while the tests ran. Finish it with release-resume.yml (see the releasing guide), and do not start the gate again." >&2
    exit 1
fi

heads=$(gh_lines "repos/$GH_REPO/git/matching-refs/heads/$pin" '.[].ref | ltrimstr("refs/heads/")')
if grep -qxF -- "$pin" <<<"$heads"; then
    echo "Deleting $pin left over from an earlier attempt."
    gh api -X DELETE "repos/$GH_REPO/git/refs/heads/$pin"
fi

if out=$(gh api "repos/$GH_REPO/git/refs" -f ref="refs/heads/$pin" -f sha="$GITHUB_SHA" 2>&1 >/dev/null); then
    exit 0
fi

# GitHub refuses the gate's token ("Resource not accessible by
# integration") when the branch has changed a workflow file since the
# tested commit: the token may not touch workflows. Nothing has been built
# or released, so starting the gate again on the new head is safe.
version=${tag#v}
if [[ $out == *"Resource not accessible by integration"* ]]; then
    if changed=$(gh api "repos/$GH_REPO/compare/$GITHUB_SHA...$branch" --jq '.files[].filename' 2>/dev/null); then
        workflows=$(grep -E '^\.github/workflows/' <<<"$changed" | paste -sd ' ' -) || true
        if [ -n "$workflows" ]; then
            echo "::error::GitHub refused to create $pin: $branch has changed workflow files since this gate started on $GITHUB_SHA ($workflows), and the gate's token may not do that then. Nothing was built or released. Start the gate again on the new $branch: gh workflow run release-gate.yml --ref $branch -f version=$version. Do not merge workflow changes into $branch until the gate has started release.yml." >&2
            exit 1
        fi
    fi
fi
echo "::error::Could not create $pin at $GITHUB_SHA: $out. Nothing was built or released; start the gate again with gh workflow run release-gate.yml --ref $branch -f version=$version." >&2
exit 1
