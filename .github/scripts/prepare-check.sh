#!/bin/bash
# Usage: prepare-check.sh <version> [<fixed-branch>]
#
# The checks release-prepare.yml makes before it creates or rebases the
# branch release-X.Y for <version> (without the "v": 1.3.0, 1.3.0-beta.1).
# Run it in a checkout of main; it needs no secret. Prints "line=X.Y",
# "branch=release-X.Y" and "fixed=" (the fixed branch, or empty), ready for
# $GITHUB_OUTPUT.
#
# release-X.Y is rebased onto main only while main is on the line, before
# X.Y.0, and while no release of the line is under way. So it refuses when:
# - the run is not on main;
# - <version> is a patch (patches are bumped on release-X.Y itself);
# - Cargo.toml and dist.toml on main do not hold <version>;
# - the tag vX.Y.0 or v<version> exists, or <version> can't follow the
#   line's releases (release-line.sh);
# - a draft release of the line, a pinned/vX.Y.* branch or a running
#   release gate exists;
# - <fixed-branch> is release-*, pinned/*, main, an odd name, or does not
#   exist on GitHub.
#
# Needs gh, logged in (GH_TOKEN), and GH_REPO. GITHUB_REF may be unset
# outside a runner.
set -euo pipefail

[ $# -eq 1 ] || [ $# -eq 2 ] || { echo "usage: $0 <version> [<fixed-branch>]" >&2; exit 2; }
version=$1
fixed=${2:-}
here=$(cd "$(dirname "$0")" && pwd)

fail() {
    echo "::error::$1" >&2
    exit 1
}

if [ -n "$fixed" ]; then
    case $fixed in
        main | release-* | pinned/* | -*)
            fail "The fixed branch can't be $fixed. Push the rebased commits to a branch of your own, such as fix-X.Y." ;;
    esac
    [[ $fixed =~ ^[A-Za-z0-9][A-Za-z0-9._/-]*$ ]] ||
        fail "The fixed branch name '$fixed' has odd characters. Use letters, digits, '.', '_', '-' and '/'."
fi

if [ -n "${GITHUB_REF-}" ] && [ "$GITHUB_REF" != refs/heads/main ]; then
    fail "Run this workflow on main: gh workflow run release-prepare.yml --ref main -f version=$version (this run is on $GITHUB_REF)."
fi

branch=$("$here/release-branch.sh" "$version")
line=${branch#release-}
tag=v$version

if [[ $version =~ ^[0-9]+\.[0-9]+\.([0-9]+)$ ]] && [ "${BASH_REMATCH[1]}" -gt 0 ]; then
    fail "$version is a patch. A patch is bumped with a pull request to $branch, not on main, and needs no preparing; start the gate there: gh workflow run release-gate.yml --ref $branch -f version=$version"
fi

if ! "$here/check-release-tag.sh" --versions "$tag" >/dev/null 2>&1; then
    main_version=$(python3 -c '
import tomllib
with open("Cargo.toml", "rb") as f:
    print(tomllib.load(f)["workspace"]["package"]["version"])
')
    if [ "$("$here/bump-branch.sh" "$version" "$main_version")" = main ]; then
        fail "main holds version $main_version, not $version. Bump it on main first: .github/scripts/bump-version.sh $version, then merge the pull request and start this workflow again."
    fi
    fail "main has moved on to $main_version, so it has left the line $line. Bump $version on $branch instead (.github/scripts/bump-version.sh $version on $branch, then gh pr create --fill --base $branch) and start the gate there: gh workflow run release-gate.yml --ref $branch -f version=$version"
fi

# Each answer goes into a variable first, then grep searches it (see
# release-start-check.sh). A failed gh call stops the script.
gh_lines() {
    gh api --paginate "$1" --jq "$2" ||
        fail "Could not read $1 from GitHub."
}

tags=$(gh_lines "repos/$GH_REPO/git/matching-refs/tags/v$line." '.[].ref | ltrimstr("refs/tags/")')
for t in "v$line.0" "$tag"; do
    if grep -qxF -- "$t" <<<"$tags"; then
        fail "The tag $t exists, so $branch is no longer rebased onto main. Bump $version on $branch with a pull request and start the gate there: gh workflow run release-gate.yml --ref $branch -f version=$version"
    fi
done

"$here/release-line.sh" "$version" >/dev/null

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

if [ -n "$fixed" ]; then
    heads=$(gh_lines "repos/$GH_REPO/git/matching-refs/heads/$fixed" '.[].ref | ltrimstr("refs/heads/")')
    grep -qxF -- "$fixed" <<<"$heads" ||
        fail "The fixed branch $fixed does not exist on GitHub. Push it first: git push origin $fixed"
fi

echo "line=$line"
echo "branch=$branch"
echo "fixed=$fixed"
