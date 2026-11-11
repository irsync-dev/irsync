#!/bin/bash
# Usage: repo-version.sh has <repo-checkout> <version>
#        repo-version.sh copy <repo-checkout> <version> <out-dir>
#
# "has" exits 0 if the repo already holds <version> as .deb and .rpm for both
# architectures, and 1 if any of the four files is missing. The packages
# workflow then skips the build and the repo update, so a re-run for a
# version that is already published changes nothing.
#
# "copy" copies those four files from the repo to <out-dir>, for attaching to
# the GitHub Release. The .rpm files in the repo are signed.
set -euo pipefail

usage() {
    echo "usage: $0 has <repo-checkout> <version>" >&2
    echo "       $0 copy <repo-checkout> <version> <out-dir>" >&2
    exit 2
}
[ $# -ge 3 ] || usage
cmd=$1
repo=$2
version=$3

files=(
    "$repo/apt/pool/main/i/irsync/irsync_${version}_amd64.deb"
    "$repo/apt/pool/main/i/irsync/irsync_${version}_arm64.deb"
    "$repo/rpm/x86_64/irsync-${version}-1.x86_64.rpm"
    "$repo/rpm/aarch64/irsync-${version}-1.aarch64.rpm"
)

case $cmd in
has)
    [ $# -eq 3 ] || usage
    for f in "${files[@]}"; do
        if [ ! -f "$f" ]; then
            echo "The repo does not have ${f#"$repo"/} yet."
            exit 1
        fi
    done
    echo "The repo already has irsync $version for every architecture."
    ;;
copy)
    [ $# -eq 4 ] || usage
    out=$4
    mkdir -p "$out"
    for f in "${files[@]}"; do
        [ -f "$f" ] || { echo "missing ${f#"$repo"/}" >&2; exit 1; }
        cp -f "$f" "$out/"
    done
    ;;
*)
    usage
    ;;
esac
