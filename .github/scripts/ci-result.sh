#!/bin/bash
# Usage: ci-result.sh '<needs-json>'   (in a workflow: '${{ toJSON(needs) }}')
#
# The one check the main branch ruleset requires. Passes when every job it
# needs succeeded or was skipped (a docs-only pull request skips the builds),
# fails when any failed or was cancelled, and names those jobs.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 '<needs-json>'" >&2; exit 2; }

bad=$(jq -r 'to_entries[] | select(.value.result != "success" and .value.result != "skipped")
             | "\(.key): \(.value.result)"' <<<"$1")
if [ -n "$bad" ]; then
    while IFS= read -r line; do echo "::error::$line" >&2; done <<<"$bad"
    exit 1
fi
echo "Every job succeeded or was skipped." >&2
