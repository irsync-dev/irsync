#!/bin/bash
# Usage: stray-tags.sh
#
# Finds stray release tags: v* tags that have no GitHub Release (draft or
# published). The release workflow creates the tag last, together with the
# release, so a v* tag without one can only have been pushed by hand.
# Prints one stray tag per line on stdout (sorted by version) and exits 1,
# with an ::error:: line on stderr. Prints nothing and exits 0 if there are none.
#
# Needs gh, logged in (GH_TOKEN), and GH_REPO.
set -euo pipefail

[ $# -eq 0 ] || { echo "usage: $0" >&2; exit 2; }

# Tags from before releases came with the tag; they never had a Release.
accepted=(v0.1.0-alpha.1 v0.1.0-alpha.2)

tags=$(gh api --paginate "repos/$GH_REPO/git/matching-refs/tags/v" \
    --jq '.[].ref | ltrimstr("refs/tags/")')
releases=$(gh api --paginate "repos/$GH_REPO/releases" --jq '.[].tag_name')

stray=()
while IFS= read -r tag; do
    [ -n "$tag" ] || continue
    skip=false
    for ok in "${accepted[@]}"; do
        [ "$tag" = "$ok" ] && skip=true
    done
    $skip && continue
    printf '%s\n' "$releases" | grep -qxF -- "$tag" || stray+=("$tag")
done < <(printf '%s\n' "$tags" | sort -V)

if [ ${#stray[@]} -gt 0 ]; then
    printf '%s\n' "${stray[@]}"
    echo "::error::Tags without a GitHub Release: ${stray[*]}. Release tags are created only by the release gate. A stray tag cannot be deleted or reused, so the next release must use a later version." >&2
    exit 1
fi
