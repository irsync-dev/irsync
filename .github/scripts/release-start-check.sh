#!/bin/bash
# Usage: release-start-check.sh <version>
#
# The checks the release gate makes before it does anything. <version> is
# written without the "v" (1.3.0, 1.3.0-beta.1). Run it in a checkout of the
# commit to release. Prints "tag=", "prerelease=", "branch=" (the branch the
# release is cut from) and "new-branch=" (release-X.Y when this release
# creates that branch, otherwise empty), ready for $GITHUB_OUTPUT.
#
# Stops at the first failed check:
# - The version is valid and Cargo.toml and dist.toml hold it.
# - The run is on the right branch (see release-branch.sh).
# - The tag does not exist yet.
# - No stray release tags exist (see stray-tags.sh).
# - X.Y.0 creates the branch release-X.Y, which must not exist yet; a patch
#   needs release-X.Y to exist.
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

if [ -n "${GITHUB_REF-}" ] && [ "$GITHUB_REF" != "refs/heads/$branch" ]; then
    echo "::error::Run this workflow on $branch: gh workflow run release-gate.yml --ref $branch -f version=$version (this run is on $GITHUB_REF)." >&2
    exit 1
fi

if gh api --paginate "repos/$GH_REPO/git/matching-refs/tags/$tag" \
        --jq '.[].ref | ltrimstr("refs/tags/")' | grep -qxF -- "$tag"; then
    if gh api --paginate "repos/$GH_REPO/releases" --jq '.[].tag_name' | grep -qxF -- "$tag"; then
        echo "::error::$tag is already released. To finish it, run: gh workflow run release-resume.yml --ref $tag -f tag=$tag" >&2
    else
        echo "::error::The tag $tag exists but has no GitHub Release: it was pushed by hand. This number can't be used; release the next version." >&2
    fi
    exit 1
fi

"$here/stray-tags.sh" >/dev/null

new_branch=
if [ "$branch" != main ] || [[ $version =~ ^[0-9]+\.[0-9]+\.0$ ]]; then
    if [ "$branch" = main ]; then
        release_branch=release-${version%.*}
    else
        release_branch=$branch
    fi
    exists=false
    if gh api --paginate "repos/$GH_REPO/git/matching-refs/heads/$release_branch" \
            --jq '.[].ref | ltrimstr("refs/heads/")' | grep -qxF -- "$release_branch"; then
        exists=true
    fi
    if [ "$branch" = main ]; then
        if $exists; then
            echo "::error::The branch $release_branch already exists, so $version can't be cut from main again. Release a patch from $release_branch, or use a later version." >&2
            exit 1
        fi
        new_branch=$release_branch
    elif ! $exists; then
        echo "::error::The branch $release_branch does not exist. Release ${release_branch#release-}.0 from main first." >&2
        exit 1
    fi
fi

echo "tag=$tag"
echo "prerelease=$prerelease"
echo "branch=$branch"
echo "new-branch=$new_branch"
