#!/bin/bash
# Usage: resume-check.sh <tag> <dry-run: true|false>
#
# Decides what release-resume.yml does for the existing release <tag>. Prints
# "packages=", "homebrew=" and "notes=" (true|false) and "new-branch="
# (release-X.Y for an X.Y.0 tag whose branch does not exist yet, else empty),
# ready for $GITHUB_OUTPUT.
#
# - The tag must be a release tag, the run must be on that tag (GITHUB_REF),
#   and its GitHub Release must exist and be published (not a draft).
# - Normal run: packages and the Homebrew formula only for a stable tag whose
#   release is not marked as a pre-release; the notes always.
# - Dry run: packages for any release tag (that is what the rehearsal on an
#   alpha is for), and no Homebrew, notes or branch, because a dry run changes
#   nothing.
#
# Needs gh, logged in (GH_TOKEN), and GH_REPO.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <tag> <true|false>" >&2; exit 2; }
tag=$1
dry_run=$2
here=$(cd "$(dirname "$0")" && pwd)

prerelease=$("$here/check-release-tag.sh" "$tag")
prerelease=${prerelease#prerelease=}

# The release secrets only open for jobs running on a v* tag, and the scripts
# should be the tagged ones. GitHub sets GITHUB_REF; outside a runner it may
# be unset.
if [ -n "${GITHUB_REF-}" ] && [ "$GITHUB_REF" != "refs/tags/$tag" ]; then
    echo "::error::Run this workflow on the tag: gh workflow run release-resume.yml --ref $tag -f tag=$tag (this run is on $GITHUB_REF)." >&2
    exit 1
fi

if ! info=$(gh release view "$tag" --json isDraft,isPrerelease \
        --jq '(.isDraft | tostring) + " " + (.isPrerelease | tostring)'); then
    echo "::error::There is no GitHub Release for $tag. This workflow only finishes a release that release.yml already published." >&2
    exit 1
fi
draft=${info% *}
marked_pre=${info#* }
if [ "$draft" != false ]; then
    echo "::error::The GitHub Release for $tag is still a draft. Publish it or delete it first." >&2
    exit 1
fi

# The branch for later patch releases, made once by the first X.Y.0 run.
new_branch=
if [ "$dry_run" != true ] && [[ $tag =~ ^v([0-9]+\.[0-9]+)\.0$ ]]; then
    candidate=release-${BASH_REMATCH[1]}
    # Into a variable, then grep -q: piped, gh can fail writing after grep
    # stopped at the match, and pipefail would turn the match into a miss.
    heads=$(gh api --paginate "repos/$GH_REPO/git/matching-refs/heads/$candidate" \
        --jq '.[].ref | ltrimstr("refs/heads/")') ||
        { echo "::error::Could not look up the branch $candidate." >&2; exit 1; }
    if ! grep -qxF -- "$candidate" <<<"$heads"; then
        new_branch=$candidate
    fi
fi

if [ "$dry_run" = true ]; then
    echo "Dry run for $tag: packages are built and signed, nothing is published." >&2
    echo "packages=true"
    echo "homebrew=false"
    echo "notes=false"
    echo "new-branch=$new_branch"
elif [ "$prerelease" = true ] || [ "$marked_pre" != false ]; then
    echo "$tag is a pre-release: no packages or Homebrew, only the notes." >&2
    echo "packages=false"
    echo "homebrew=false"
    echo "notes=true"
    echo "new-branch=$new_branch"
else
    echo "packages=true"
    echo "homebrew=true"
    echo "notes=true"
    echo "new-branch=$new_branch"
fi
