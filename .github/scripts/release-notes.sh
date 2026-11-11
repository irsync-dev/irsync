#!/bin/bash
# Usage: release-notes.sh <tag>
#
# Adds GitHub's generated list of changes to the notes of the release <tag>,
# under the heading "## What's changed". If the heading is already there, it
# does nothing, so it is safe to run again.
#
# For a stable tag the list starts at the newest earlier stable tag, so it
# covers the betas and alphas in between too. Without that, GitHub would start
# at the newest earlier tag of any kind. Pre-releases keep GitHub's default.
#
# Needs gh, logged in (GH_TOKEN), and GH_REPO set to owner/name.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <tag>" >&2; exit 2; }
tag=$1
: "${GH_REPO:?GH_REPO must be set}"
heading="## What's changed"

body=$(gh release view "$tag" --json body --jq .body)
if grep -qxF "$heading" <<<"$body"; then
    echo "The release notes already have '$heading'. Nothing to do."
    exit 0
fi

args=(-f tag_name="$tag")
stable_re='^v[0-9]+\.[0-9]+\.[0-9]+$'
if [[ $tag =~ $stable_re ]]; then
    tags=$(gh api --paginate "repos/$GH_REPO/git/matching-refs/tags/v" \
        --jq '.[].ref | ltrimstr("refs/tags/")')
    # Sort the stable tags and this one by version; take the one just before.
    previous=$( { grep -E "$stable_re" <<<"$tags" || true; echo "$tag"; } |
        sort -V -u | awk -v tag="$tag" '$0 == tag { print prev; exit } { prev = $0 }')
    if [ -n "$previous" ]; then
        echo "Listing the changes since $previous."
        args+=(-f previous_tag_name="$previous")
    else
        echo "No earlier stable tag. Listing all changes up to $tag."
    fi
fi

generated=$(gh api "repos/$GH_REPO/releases/generate-notes" "${args[@]}" --jq .body)
if [ -z "$generated" ]; then
    echo "::error::GitHub returned an empty change list for $tag." >&2
    exit 1
fi

notes=$(mktemp)
trap 'rm -f "$notes"' EXIT
{
    printf '%s' "$body"
    [ -n "$body" ] && printf '\n\n'
    printf '%s\n\n%s\n' "$heading" "$generated"
} > "$notes"
gh release edit "$tag" --notes-file "$notes"
