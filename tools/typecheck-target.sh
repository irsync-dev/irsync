#!/bin/sh
# Type-checks irsync for another platform, on a machine with no C compiler
# for it.
#
#   tools/typecheck-target.sh TARGET [cargo clippy arguments]
#   tools/typecheck-target.sh x86_64-unknown-linux-gnu
#   tools/typecheck-target.sh aarch64-apple-darwin -p irsync --all-targets --no-default-features
#
# irsync has dependencies with C code, so `cargo check --target TARGET`
# stops when the build scripts find no C compiler for TARGET. A check never
# links, though: it only needs those build scripts to finish. This script
# hands them a stand-in compiler and a stand-in archiver
# (tools/typecheck-stubs) that write empty files, and runs clippy with
# warnings as errors.
#
# What it finds: code that does not compile or lint on TARGET, in the crate
# and in every test. What it cannot do: build or run anything. A check that
# passes here says nothing about behaviour; CI still has to run the tests.
#
# The Rust target must be installed: rustup target add TARGET.
#
# The results go into their own target directory (target/typecheck, or
# TYPECHECK_TARGET_DIR), so the empty objects never mix with a real build.
set -eu

if [ $# -lt 1 ]; then
    echo "usage: tools/typecheck-target.sh TARGET [cargo clippy arguments]" >&2
    exit 2
fi
target=$1
shift

stubs=$(cd "$(dirname "$0")/typecheck-stubs" && pwd)
# In Git Bash, cargo is a Windows program and needs a Windows path.
if command -v cygpath >/dev/null 2>&1; then
    stubs=$(cygpath -m "$stubs")
fi
case $stubs in
    *" "*) echo "typecheck-target.sh: the checkout path holds a space, which the compiler setting cannot carry" >&2; exit 2 ;;
esac

# The cc crate reads CC_<target> and AR_<target>, with - written as _.
key=$(printf '%s' "$target" | tr - _)
export "CC_$key=sh $stubs/cc.sh"
export "CXX_$key=sh $stubs/cc.sh"
export "AR_$key=sh $stubs/ar.sh"
export CARGO_TARGET_DIR="${TYPECHECK_TARGET_DIR:-target/typecheck}"

if [ $# -eq 0 ]; then
    set -- -p irsync -p irsync-proto --all-targets --all-features
fi
exec cargo clippy --locked "$@" --target "$target" -- -D warnings
