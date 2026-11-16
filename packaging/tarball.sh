#!/bin/sh
# Packs irsync-TARGET.tar.xz, laid out like the tarballs dist puts on a
# release: one folder, irsync-TARGET, with irsync, fileset, README.md and
# LICENSE. Writes irsync-TARGET.tar.xz.sha256 next to it.
#
# Usage: packaging/tarball.sh TARGET BIN_DIR OUT_DIR
#
# BIN_DIR holds the two built binaries. OUT_DIR is created if needed.
#
# `tools/ci.sh freebsd-build` runs this: dist cannot build on FreeBSD yet,
# so the FreeBSD build packs its own tarball. The release targets don't use
# it; dist packs those.
set -eu

die() {
    echo "tarball.sh: $*" >&2
    exit 1
}

[ $# -eq 3 ] || die "usage: packaging/tarball.sh TARGET BIN_DIR OUT_DIR"
target=$1
bin_dir=$2
out_dir=$3

repo_root=$(cd "$(dirname "$0")/.." && pwd)

# Check every input before writing anything.
for bin in irsync fileset; do
    [ -f "$bin_dir/$bin" ] || die "$bin_dir/$bin not found"
    [ -x "$bin_dir/$bin" ] || die "$bin_dir/$bin is not executable"
done
for doc in README.md LICENSE; do
    [ -f "$repo_root/$doc" ] || die "$repo_root/$doc not found"
done

name="irsync-$target"
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

mkdir "$stage/$name"
cp "$bin_dir/irsync" "$bin_dir/fileset" "$repo_root/README.md" "$repo_root/LICENSE" "$stage/$name/"

# Pack in the temp folder and move the result over, so a failed run leaves
# no half-written tarball. -cJf works with GNU tar and FreeBSD's tar.
tar -cJf "$stage/$name.tar.xz" -C "$stage" "$name"

if command -v sha256sum >/dev/null 2>&1; then
    digest=$(sha256sum "$stage/$name.tar.xz" | cut -d' ' -f1)
else
    # FreeBSD before 14 has only this one.
    digest=$(sha256 -q "$stage/$name.tar.xz")
fi
[ ${#digest} -eq 64 ] || die "could not work out the SHA-256 of the tarball"

mkdir -p "$out_dir"
mv "$stage/$name.tar.xz" "$out_dir/$name.tar.xz"
# The format `sha256sum -c` reads.
printf '%s  %s\n' "$digest" "$name.tar.xz" > "$out_dir/$name.tar.xz.sha256"
echo "tarball.sh: wrote $out_dir/$name.tar.xz"
