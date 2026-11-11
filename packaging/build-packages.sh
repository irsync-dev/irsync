#!/bin/sh
# Usage: build-packages.sh <archives-dir> <version> <out-dir>
# Turns the Linux release archives into .deb and .rpm packages.
set -eu

[ $# -eq 3 ] || { echo "usage: $0 <archives-dir> <version> <out-dir>" >&2; exit 2; }
archives=$1
version=$2
out=$3
here=$(cd "$(dirname "$0")" && pwd)

mkdir -p "$out"
out=$(cd "$out" && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

for pair in x86_64:amd64:x86_64 aarch64:arm64:aarch64; do
    triple_arch=${pair%%:*}
    rest=${pair#*:}
    deb_arch=${rest%%:*}
    rpm_arch=${rest#*:}
    archive="$archives/irsync-$triple_arch-unknown-linux-musl.tar.xz"
    [ -f "$archive" ] || { echo "missing $archive" >&2; exit 1; }

    dir="$work/$triple_arch"
    mkdir -p "$dir"
    tar -xJf "$archive" -C "$dir"
    # The archive holds one top-level directory; tolerate none as well.
    bin=$(dirname "$(find "$dir" -type f -name irsync | head -n 1)")
    [ -f "$bin/irsync" ] && [ -f "$bin/fileset" ] || {
        echo "$archive: irsync and fileset not found" >&2; exit 1; }

    export IRSYNC_VERSION="$version" IRSYNC_ARCH="$deb_arch" IRSYNC_BIN_DIR="$bin"
    IRSYNC_RELEASE= nfpm package --config "$here/nfpm.yaml" --packager deb \
        --target "$out/irsync_${version}_${deb_arch}.deb"
    IRSYNC_RELEASE=1 nfpm package --config "$here/nfpm.yaml" --packager rpm \
        --target "$out/irsync-${version}-1.${rpm_arch}.rpm"
done
