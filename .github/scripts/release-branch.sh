#!/bin/bash
# Usage: release-branch.sh <version>
#
# Prints the branch a release is cut from: release-X.Y, for every release of
# the line X.Y (alpha, beta, X.Y.0 and its patches). <version> is written
# without the "v" (1.3.0, 1.3.0-beta.1). The release gate runs on that branch.
#
# Where the version bump goes before that is bump-branch.sh's rule. This is
# the one place the rule for the release branch lives. Errors go to stderr.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
version=$1
here=$(cd "$(dirname "$0")" && pwd)

if [[ $version == v* ]]; then
    echo "::error::Give the version without the v: ${version#v}, not $version." >&2
    exit 1
fi
"$here/check-release-tag.sh" "v$version" >/dev/null

[[ $version =~ ^([0-9]+)\.([0-9]+)\. ]]
echo "release-${BASH_REMATCH[1]}.${BASH_REMATCH[2]}"
