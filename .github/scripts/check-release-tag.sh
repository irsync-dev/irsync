#!/bin/bash
# Usage: check-release-tag.sh [--versions] <tag>
#
# Checks that <tag> is a release tag: vX.Y.Z, vX.Y.Z-beta.N or vX.Y.Z-alpha.N.
# Odd tag names must never reach release.yml, which puts the tag into shell
# commands.
#
# With --versions it also checks that the tag without the "v" equals the
# version in Cargo.toml ([workspace.package]) and in packaging/dist/dist.toml,
# both read from the current directory.
#
# Prints "prerelease=true" or "prerelease=false", ready for $GITHUB_OUTPUT.
# Errors go to stderr (GitHub shows ::error:: lines from there too).
set -euo pipefail

versions=false
if [ "${1:-}" = --versions ]; then
    versions=true
    shift
fi
[ $# -eq 1 ] || { echo "usage: $0 [--versions] <tag>" >&2; exit 2; }
tag=$1

tag_re='^v[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta)\.[0-9]+)?$'
if ! [[ $tag =~ $tag_re ]]; then
    echo "::error::Tag '$tag' is not a release tag. Use vX.Y.Z, vX.Y.Z-beta.N or vX.Y.Z-alpha.N." >&2
    exit 1
fi

if $versions; then
    want=${tag#v}
    read_version() {
        python3 -c '
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    data = tomllib.load(f)
for key in sys.argv[2:]:
    data = data[key]
print(data)
' "$@"
    }
    cargo=$(read_version Cargo.toml workspace package version)
    dist=$(read_version packaging/dist/dist.toml package version)
    bad=false
    if [ "$cargo" != "$want" ]; then
        echo "::error::Cargo.toml has version $cargo, but the tag $tag needs $want." >&2
        bad=true
    fi
    if [ "$dist" != "$want" ]; then
        echo "::error::packaging/dist/dist.toml has version $dist, but the tag $tag needs $want." >&2
        bad=true
    fi
    if $bad; then
        exit 1
    fi
fi

case $tag in
    *-*) echo "prerelease=true" ;;
    *) echo "prerelease=false" ;;
esac
