#!/bin/bash
# Usage: resume-check.sh <tag> <dry-run: true|false>
#
# Decides what release-resume.yml does for the existing release <tag>. Prints
# "packages=true|false" and "notes=true|false", ready for $GITHUB_OUTPUT.
#
# - The tag must be a release tag, and its GitHub Release must exist and be
#   published (not a draft).
# - Normal run: packages only for a stable tag whose release is not marked as
#   a pre-release; the notes always.
# - Dry run: packages for any release tag (that is what the rehearsal on an
#   alpha is for), and no notes, because a dry run changes nothing.
#
# Needs gh, logged in (GH_TOKEN), and GH_REPO.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <tag> <true|false>" >&2; exit 2; }
tag=$1
dry_run=$2
here=$(cd "$(dirname "$0")" && pwd)

prerelease=$("$here/check-release-tag.sh" "$tag")
prerelease=${prerelease#prerelease=}

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

if [ "$dry_run" = true ]; then
    echo "Dry run for $tag: packages are built and signed, nothing is published." >&2
    echo "packages=true"
    echo "notes=false"
elif [ "$prerelease" = true ] || [ "$marked_pre" != false ]; then
    echo "$tag is a pre-release: no packages, only the notes." >&2
    echo "packages=false"
    echo "notes=true"
else
    echo "packages=true"
    echo "notes=true"
fi
