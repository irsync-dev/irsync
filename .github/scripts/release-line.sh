#!/bin/bash
# Usage: release-line.sh <version>
#
# Checks that <version> (without the "v": 1.3.0, 1.3.0-beta.1) may be
# released next on its line X.Y, given the release tags vX.Y.* on GitHub.
# Every line lives on one branch, release-X.Y, and goes through alpha.N,
# then beta.N, then X.Y.0 and its patches:
#
# - An alpha follows a lower alpha.
# - A beta follows an alpha or a lower beta.
# - A stable release follows an alpha, a beta or a lower stable release.
# - Pre-releases are only of X.Y.0.
# - A line may start with a beta or a stable release. That is allowed, with
#   a ::warning:: that it is not the usual order.
#
# Everything else is refused with an ::error:: and exit 1. Tags without a
# GitHub Release (the old v0.1.0-alpha.1 and .2) count too.
#
# Prints "line=X.Y" and "first=true|false" (no release on the line yet),
# ready for $GITHUB_OUTPUT. Needs gh, logged in (GH_TOKEN), and GH_REPO.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: $0 <version>" >&2; exit 2; }
version=$1
here=$(cd "$(dirname "$0")" && pwd)

if [[ $version == v* ]]; then
    echo "::error::Give the version without the v: ${version#v}, not $version." >&2
    exit 1
fi
"$here/check-release-tag.sh" "v$version" >/dev/null

[[ $version =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)(-.*)?$ ]]
line=${BASH_REMATCH[1]}.${BASH_REMATCH[2]}
if [ -n "${BASH_REMATCH[4]}" ] && [ "${BASH_REMATCH[3]}" != 0 ]; then
    echo "::error::$version is a pre-release of a patch. Pre-releases are only of X.Y.0; release $line.${BASH_REMATCH[3]} directly." >&2
    exit 1
fi

endpoint="repos/$GH_REPO/git/matching-refs/tags/v$line."
tags=$(gh api --paginate "$endpoint" --jq '.[].ref | ltrimstr("refs/tags/")') ||
    { echo "::error::Could not read $endpoint from GitHub." >&2; exit 1; }

python3 - "$version" "$line" "$tags" <<'EOF'
import re
import sys

version, line, tags = sys.argv[1:4]
pattern = re.compile(r"(\d+)\.(\d+)\.(\d+)(?:-(alpha|beta)\.(\d+))?")
RANK = {"alpha": 0, "beta": 1, None: 2}


def key(v):
    m = pattern.fullmatch(v)
    return (int(m[3]), RANK[m[4]], int(m[5] or 0))


mine = []
for tag in tags.split():
    m = pattern.fullmatch(tag[1:]) if tag.startswith("v") else None
    if m and f"{m[1]}.{m[2]}" == line:
        mine.append(tag[1:])

new = key(version)
if not mine:
    if new[1] > 0:
        print(f"::warning::{version} is the first release of line {line}. "
              "A line usually starts with an alpha; starting with a "
              f"{'beta' if new[1] == 1 else 'stable release'} is not the usual order.",
              file=sys.stderr)
    print(f"line={line}\nfirst=true")
    sys.exit(0)

newest = max(mine, key=key)
old = key(newest)
if new <= old or new[1] < old[1]:
    print(f"::error::{version} can't follow v{newest}, the newest release of line {line}. "
          "An alpha follows a lower alpha, a beta follows an alpha or a lower beta, "
          "and a stable release follows any lower release.", file=sys.stderr)
    sys.exit(1)
print(f"line={line}\nfirst=false")
EOF
