#!/bin/bash
# Usage: changelog-section.sh <version>
#
# Prints the release notes of <version> (written without the "v": 1.3.0,
# 1.3.0-beta.1) from CHANGELOG.md in the current directory: the text under
# the heading "## v<version>", up to the next "## " heading.
#
# Fails when the file or the heading is missing, when the heading is there
# twice, or when nothing is written under it. dist puts this section at the
# top of the GitHub Release (it reads CHANGELOG.md at the root of the repo),
# so a release must not be cut without it. The release gate runs this check
# (release-start-check.sh), and a test does for the workspace's version
# (crates/irsync/tests/release_version.rs).
#
# Errors go to stderr (GitHub shows ::error:: lines from there too).
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
version=$1
file=CHANGELOG.md
heading="## v$version"

if [ ! -f "$file" ]; then
    echo "::error::There is no $file. The release notes of $version go there, under the heading '$heading'." >&2
    exit 1
fi

count=$(grep -cxF -- "$heading" "$file" || true)
if [ "$count" -eq 0 ]; then
    echo "::error::$file has no section '$heading'. Write the release notes of $version there (see AGENTS.md, Release notes)." >&2
    exit 1
fi
if [ "$count" -gt 1 ]; then
    echo "::error::$file has the heading '$heading' $count times. Keep one." >&2
    exit 1
fi

section=$(awk -v heading="$heading" '
    $0 == heading { inside = 1; next }
    inside && /^## / { exit }
    inside { print }
' "$file")
if ! grep -q '[^[:space:]]' <<<"$section"; then
    echo "::error::The section '$heading' of $file is empty. Write the release notes of $version there (see AGENTS.md, Release notes)." >&2
    exit 1
fi
printf '%s\n' "$section"
