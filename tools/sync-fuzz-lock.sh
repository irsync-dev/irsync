#!/bin/sh
# Keeps fuzz/Cargo.lock in step with Cargo.lock.
#
# Usage: tools/sync-fuzz-lock.sh [--check]
#
# fuzz/ is a project of its own with its own lock file, but it builds the
# workspace's crates, so it should build them with the same dependencies.
# Run this after any change to Cargo.lock.
#
# With no argument it writes fuzz/Cargo.lock again: it starts from a copy of
# Cargo.lock and lets cargo drop what the fuzz targets do not use and add
# what only they use (libfuzzer-sys and what it needs). cargo keeps every
# version the copy names, so the shared crates end up the same. The crates
# only the fuzz targets use move to their newest version.
#
# With --check it changes nothing and fails if
#   - fuzz/Cargo.lock does not fit fuzz/Cargo.toml and the workspace's
#     crates (cargo would have to rewrite it), or
#   - a crate in both lock files has a version or a source in
#     fuzz/Cargo.lock that Cargo.lock does not have.
# It needs no nightly: it runs with the workspace's own Rust.
set -eu

check=0
case "${1:-}" in
    "") ;;
    --check) check=1 ;;
    *)
        echo "usage: $0 [--check]" >&2
        exit 2
        ;;
esac
if [ $# -gt 1 ]; then
    echo "usage: $0 [--check]" >&2
    exit 2
fi

# From the top, so that cargo uses the Rust in rust-toolchain.toml there and
# not the nightly in fuzz/rust-toolchain.toml.
root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$root"

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# One line for each package in a lock file: name, version, source. A crate
# from this repository has no source.
packages() {
    awk '
        function flush() { if (n != "") print n, v, s; n = ""; v = ""; s = "" }
        /^\[\[package\]\]/ { flush() }
        /^name = / { n = $3 }
        /^version = / { v = $3 }
        /^source = / { s = $3 }
        END { flush() }
    ' "$1" | tr -d '"'
}

# The packages of fuzz/Cargo.lock that Cargo.lock has under the same name,
# but not at that version from that source.
differing() {
    packages Cargo.lock >"$tmp/main"
    packages fuzz/Cargo.lock >"$tmp/fuzz"
    awk '
        NR == FNR { names[$1]; seen[$0]; next }
        ($1 in names) && !($0 in seen) { print "  " $1 " " $2 }
    ' "$tmp/main" "$tmp/fuzz"
}

if [ "$check" -eq 0 ]; then
    cp Cargo.lock fuzz/Cargo.lock
    cargo metadata --format-version 1 --manifest-path fuzz/Cargo.toml >/dev/null
fi

if ! cargo metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml >/dev/null; then
    echo "sync-fuzz-lock.sh: fuzz/Cargo.lock is out of date; run tools/sync-fuzz-lock.sh and commit the result" >&2
    exit 1
fi

diff=$(differing)
if [ -n "$diff" ]; then
    {
        echo "sync-fuzz-lock.sh: fuzz/Cargo.lock has these crates at a version or from a source that Cargo.lock does not have:"
        echo "$diff"
        if [ "$check" -eq 1 ]; then
            echo "run tools/sync-fuzz-lock.sh and commit the result"
        else
            echo "fuzz/Cargo.toml probably asks for something the workspace does not: compare its [dependencies] and [patch] with Cargo.toml"
        fi
    } >&2
    exit 1
fi
