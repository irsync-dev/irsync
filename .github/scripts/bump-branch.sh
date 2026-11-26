#!/bin/bash
# Usage: bump-branch.sh <version> <main-version>
#
# Prints the branch the version bump of a release goes to, by pull request.
# <version> is the release, <main-version> the version main holds now, both
# without the "v".
#
# - A patch X.Y.Z (Z > 0) is bumped on release-X.Y.
# - A pre-release or X.Y.0 is bumped on main, and release-prepare.yml then
#   rebases release-X.Y onto main. Once main is on a later line than X.Y,
#   main has left the line, and the bump goes to release-X.Y instead.
#
# The first release of a line is bumped on main too, while main still holds
# the old line's version. This is the one place that rule lives. Errors go
# to stderr.
set -euo pipefail

[ $# -eq 2 ] || { echo "usage: $0 <version> <main-version>" >&2; exit 2; }
version=$1
main_version=$2
here=$(cd "$(dirname "$0")" && pwd)

release_branch=$("$here/release-branch.sh" "$version")
"$here/check-release-tag.sh" "v$main_version" >/dev/null

[[ $version =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-.*)?$ ]]
x=${BASH_REMATCH[1]} y=${BASH_REMATCH[2]} z=${BASH_REMATCH[3]} pre=${BASH_REMATCH[4]}
[[ $main_version =~ ^([0-9]+)\.([0-9]+)\. ]]
mx=${BASH_REMATCH[1]} my=${BASH_REMATCH[2]}

if [ -z "$pre" ] && [ "$z" -gt 0 ]; then
    echo "$release_branch"
elif [ "$mx" -gt "$x" ] || { [ "$mx" -eq "$x" ] && [ "$my" -gt "$y" ]; }; then
    echo "$release_branch"
else
    echo main
fi
