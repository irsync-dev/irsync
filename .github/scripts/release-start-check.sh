#!/bin/bash
# Usage: release-start-check.sh <version>
#
# The checks the release gate makes before it does anything. <version> is
# written without the "v" (1.3.0, 1.3.0-beta.1). Run it in a checkout of the
# commit to release. Prints "tag=", "prerelease=" and "branch=" (release-X.Y,
# the branch every release of the line X.Y is cut from), ready for
# $GITHUB_OUTPUT.
#
# Stops at the first failed check:
# - The version is valid and Cargo.toml and dist.toml hold it.
# - CHANGELOG.md has the release notes of the version (see
#   changelog-section.sh).
# - The run is on the right branch (see release-branch.sh).
# - The tag does not exist yet.
# - No stray release tags exist (see stray-tags.sh).
# - The branch release-X.Y exists. release-prepare.yml creates it, for a
#   pre-release or X.Y.0; this gate never creates branches.
# - The version may follow the line's earlier releases (release-line.sh; a
#   line that starts with a beta or stable passes with a warning).
#
# Needs gh, logged in (GH_TOKEN), and GH_REPO. GITHUB_REF may be unset
# outside a runner.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
version=$1
here=$(cd "$(dirname "$0")" && pwd)

branch=$("$here/release-branch.sh" "$version")
tag=v$version
prerelease=$("$here/check-release-tag.sh" --versions "$tag")
prerelease=${prerelease#prerelease=}
"$here/changelog-section.sh" "$version" >/dev/null

if [ -n "${GITHUB_REF-}" ] && [ "$GITHUB_REF" != "refs/heads/$branch" ]; then
    echo "::error::Run this workflow on $branch: gh workflow run release-gate.yml --ref $branch -f version=$version (this run is on $GITHUB_REF)." >&2
    exit 1
fi

# Each answer goes into a variable first, then grep -q searches it. Piped
# into grep -q, gh can fail writing after grep has found the match and
# stopped reading, and pipefail then turns the match into a miss. A failed
# gh call stops the script; it must not read as "not there".
gh_lines() {
    gh api --paginate "$1" --jq "$2" ||
        { echo "::error::Could not read $1 from GitHub." >&2; exit 1; }
}
tags=$(gh_lines "repos/$GH_REPO/git/matching-refs/tags/$tag" '.[].ref | ltrimstr("refs/tags/")')
if grep -qxF -- "$tag" <<<"$tags"; then
    releases=$(gh_lines "repos/$GH_REPO/releases" '.[].tag_name')
    if grep -qxF -- "$tag" <<<"$releases"; then
        echo "::error::$tag is already released. To finish it, run: gh workflow run release-resume.yml --ref $tag -f tag=$tag" >&2
    else
        echo "::error::The tag $tag exists but has no GitHub Release: it was pushed by hand. This number can't be used; release the next version." >&2
    fi
    exit 1
fi

"$here/stray-tags.sh" >/dev/null

heads=$(gh_lines "repos/$GH_REPO/git/matching-refs/heads/$branch" '.[].ref | ltrimstr("refs/heads/")')
if ! grep -qxF -- "$branch" <<<"$heads"; then
    if [[ $version =~ ^[0-9]+\.[0-9]+\.0(-.*)?$ ]]; then
        echo "::error::The branch $branch does not exist. Create it first: gh workflow run release-prepare.yml --ref main -f version=$version" >&2
    else
        echo "::error::The branch $branch does not exist. A patch needs it: release ${branch#release-}.0 first." >&2
    fi
    exit 1
fi

"$here/release-line.sh" "$version" >/dev/null

echo "tag=$tag"
echo "prerelease=$prerelease"
echo "branch=$branch"
