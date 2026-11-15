#!/bin/bash
# Usage: release-branch.sh <version>
#
# Prints the branch a release is cut from. <version> is written without the
# "v" (1.3.0, 1.3.0-beta.1).
#
# - Pre-releases and X.Y.0 come from main.
# - A patch X.Y.Z (Z > 0) comes from release-X.Y.
#
# This is the one place that rule lives. Errors go to stderr.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
version=$1
here=$(cd "$(dirname "$0")" && pwd)

if [[ $version == v* ]]; then
    echo "::error::Give the version without the v: ${version#v}, not $version." >&2
    exit 1
fi
"$here/check-release-tag.sh" "v$version" >/dev/null

if [[ $version =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]] && [ "${BASH_REMATCH[3]}" -gt 0 ]; then
    echo "release-${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"
else
    echo "main"
fi
