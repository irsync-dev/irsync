#!/bin/sh
# Build script for the generic `irsync` dist package (see dist.toml).
#
# cargo dist runs this from packaging/dist with CARGO_DIST_TARGET set to the
# target triple. It builds the `irsync` and `fileset` crates from the repo
# root, copies both binaries here (where dist picks up a generic package's
# binaries), and refuses to hand over a broken binary.
set -eu

: "${CARGO_DIST_TARGET:?CARGO_DIST_TARGET must be set (dist sets it)}"

pkg_dir=$(cd "$(dirname "$0")" && pwd)
repo_root=$(cd "$pkg_dir/../.." && pwd)

version=$(sed -n 's/^version = "\(.*\)"$/\1/p' "$pkg_dir/dist.toml")
if [ -z "$version" ]; then
    echo "build.sh: no version found in $pkg_dir/dist.toml" >&2
    exit 1
fi

rustup target add "$CARGO_DIST_TARGET"

# Run from the repo root, a real git checkout, so irsync's build.rs bakes in
# the commit. Default features only: never netsim or test-hooks.
cd "$repo_root"
cargo build --locked --profile dist --target "$CARGO_DIST_TARGET" -p irsync -p fileset

out="$repo_root/target/$CARGO_DIST_TARGET/dist"
for bin in irsync fileset; do
    cp "$out/$bin" "$pkg_dir/$bin"
done

cd "$pkg_dir"

case "$CARGO_DIST_TARGET" in
    *-linux-musl)
        for bin in irsync fileset; do
            info=$(file -L "$bin")
            case "$info" in
                *"statically linked"* | *"static-pie linked"*) ;;
                *)
                    echo "build.sh: $bin is not statically linked: $info" >&2
                    exit 1
                    ;;
            esac
        done
        ;;
esac

# Every release runner builds for its own platform, so the binary runs here.
reported=$(./irsync --version)
case "$reported" in
    *unknown*)
        echo "build.sh: irsync --version reports an unknown build: $reported" >&2
        exit 1
        ;;
esac
case "$reported" in
    *"$version"*) ;;
    *)
        echo "build.sh: irsync --version ($reported) does not contain $version" >&2
        exit 1
        ;;
esac
echo "build.sh: built $reported"
