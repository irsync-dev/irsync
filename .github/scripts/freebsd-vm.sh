#!/bin/sh
# Tests irsync and builds its tarball inside a FreeBSD VM.
#
# .github/workflows/freebsd.yml runs this, with no arguments, from the
# checkout, as root (the VM action runs everything as root). After a
# passing run the tarball and its .sha256 are in freebsd-out/ in the
# directory it was started from.
#
# It has two halves. As root it installs the packages, makes an ordinary
# user and runs itself again as that user. As that user it installs Rust
# and runs the checks: the tests expect an ordinary user (the daemon
# refuses to start as root).
#
# On a FreeBSD machine of your own you don't need this script: install the
# packages below, Rust and cargo-nextest, then run
# `tools/ci.sh check freebsd-build`.
set -eu

# Keep equal to cargo-nextest in ci.yml.
NEXTEST_VERSION=0.9.151

user=ci
work=/home/$user/irsync
tarball=irsync-x86_64-unknown-freebsd.tar.xz

if [ "$(id -u)" -eq 0 ]; then
    start=$(pwd)

    # bash: tools/ci.sh. git: the build bakes in the commit. python3: the
    # dev-setup tests. rsync: the comparison test. hs-pandoc: the man pages
    # (mandoc, which checks them, is part of FreeBSD). If the build asks
    # for another package, add it here.
    pkg install -y bash curl git hs-pandoc python3 rsync rustup-init

    if ! pw usershow "$user" >/dev/null 2>&1; then
        pw useradd "$user" -m
    fi

    # Build on the VM's own disk, outside the folder the VM action syncs:
    # it would copy gigabytes of target/ back to the runner.
    rm -rf "$work"
    cp -R "$start" "$work"
    chown -R "$user" "$work"

    # `su -l` starts with an empty environment, so set what the checks
    # need. CI makes tests fail where they would otherwise skip (no usable
    # rsync, say).
    su -l "$user" -c "cd '$work' && env CI=true RUST_BACKTRACE=1 CARGO_TERM_COLOR=always .github/scripts/freebsd-vm.sh"

    mkdir -p "$start/freebsd-out"
    cp "$work/target/distrib/$tarball" "$work/target/distrib/$tarball.sha256" "$start/freebsd-out/"
    echo "freebsd-vm.sh: the tarball is in freebsd-out/"
    exit 0
fi

# From here on: the ordinary user, in its own copy of the checkout.

uname -a

# rustup, then the Rust version in rust-toolchain.toml: the one every other
# platform is tested with.
rustup-init -y --default-toolchain none --no-modify-path
PATH="$HOME/.cargo/bin:$PATH"
export PATH
# Named here, with the components from rust-toolchain.toml: a bare
# `rustup toolchain install` reads that file only from rustup 1.28 on.
channel=$(sed -n 's/^channel = "\(.*\)"$/\1/p' rust-toolchain.toml)
if [ -z "$channel" ]; then
    echo "freebsd-vm.sh: no channel found in rust-toolchain.toml" >&2
    exit 1
fi
rustup toolchain install "$channel" --profile minimal --component rustfmt --component clippy
rustc --version

# No ready-made binary is fetched here; this builds it, which takes a few
# minutes.
cargo install --locked "cargo-nextest@$NEXTEST_VERSION"

rsync --version | head -1

# Temp files in a folder of this user's own, as on macOS. On FreeBSD a new
# file takes its folder's group, and /tmp belongs to wheel, which this user
# is not in: tests that set a file's group or its setgid bit would fail.
mkdir -p "$HOME/tmp"
TMPDIR=$HOME/tmp
export TMPDIR

tools/ci.sh check freebsd-build
