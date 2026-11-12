#!/usr/bin/env bash
# Sets up a development machine for this repo.
#
# Usage: tools/dev-setup.sh [--check]
#
# Installs what rustup and cargo can install: rustup itself, the toolchain
# from rust-toolchain.toml with rustfmt and clippy, the musl targets on Linux,
# and cargo-nextest, dist and mdbook at the versions CI uses. Points git at
# the repo's hooks. Checks the rest, which comes from the system package
# manager, and says which package to install. Safe to run again.
#
# --check changes nothing; it only reports. The exit status is 1 when
# something required is missing, else 0. Missing optional tools only warn.
set -euo pipefail
# With pipefail, never end a pipe in a reader that stops early (head -1,
# grep -q): the writer then dies of SIGPIPE and the pipe fails. Use
# sed -n 1p and grep >/dev/null, which read to the end.

root=$(cd "$(dirname "$0")/.." && pwd -P)
check_only=false
case ${1:-} in
    "") ;;
    --check) check_only=true ;;
    -h | --help) sed -n '2,13p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

os=$(uname -s)
failed=0
ok() { printf '  ok       %s\n' "$*"; }
warn() { printf '  missing  %s (optional)\n' "$*"; }
bad() { printf '  missing  %s (required)\n' "$*"; failed=1; }
step() { printf '%s\n' "$*"; }
run() {
    printf '  running  %s\n' "$*"
    "$@"
}

# True if version $1 >= $2 (dotted numbers).
version_ge() {
    awk -v a="$1" -v b="$2" 'BEGIN {
        n = split(a, x, "."); m = split(b, y, ".");
        for (i = 1; i <= (n > m ? n : m); i++) {
            if (x[i] + 0 > y[i] + 0) exit 0;
            if (x[i] + 0 < y[i] + 0) exit 1;
        }
        exit 0 }'
}

# The package that provides a system tool, for the "missing" message.
hint() {
    if [ "$os" = Darwin ]; then
        case $1 in
            cc | git) echo "run: xcode-select --install" ;;
            python3) echo "brew install python" ;;
            node) echo "brew install node" ;;
            *) echo "brew install $1" ;;
        esac
    else
        case $1 in
            cc) echo "apt install build-essential" ;;
            musl-gcc) echo "apt install musl-tools" ;;
            node) echo "apt install nodejs npm" ;;
            apt-ftparchive) echo "apt install apt-utils" ;;
            createrepo_c) echo "apt install createrepo-c" ;;
            rpmsign) echo "apt install rpm" ;;
            gpg) echo "apt install gnupg" ;;
            dpkg-deb) echo "apt install dpkg" ;;
            nfpm) echo "see .github/scripts/install-packaging-tools.sh" ;;
            *) echo "apt install $1" ;;
        esac
    fi
}

have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- Rust
step "Rust (installed with rustup and cargo)"

# rustup and cargo install into $CARGO_HOME/bin, ~/.cargo/bin by default.
cargo_bin=${CARGO_HOME:-$HOME/.cargo}/bin
if ! have rustup && [ -x "$cargo_bin/rustup" ]; then
    PATH="$cargo_bin:$PATH"
fi
if have rustup; then
    ok "rustup"
elif $check_only; then
    bad "rustup: run tools/dev-setup.sh without --check to install it"
else
    have curl || { bad "curl, to download rustup: $(hint curl)"; exit 1; }
    # The official installer. It adds ~/.cargo/bin to the shell profile.
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs |
        sh -s -- -y --default-toolchain none
    PATH="$cargo_bin:$PATH"
    ok "rustup (installed; open a new shell to have it on PATH)"
fi

rust_ready=false
if have rustup; then
    channel=$(sed -n 's/^channel *= *"\(.*\)"/\1/p' "$root/rust-toolchain.toml")
    need=$(sed -n 's/^rust-version *= *"\(.*\)"/\1/p' "$root/Cargo.toml")
    toolchain_ok() {
        rustup run "$channel" rustc --version >/dev/null 2>&1 &&
            rustup component list --installed --toolchain "$channel" 2>/dev/null |
            grep '^rustfmt' >/dev/null &&
            rustup component list --installed --toolchain "$channel" 2>/dev/null |
            grep '^clippy' >/dev/null
    }
    if ! toolchain_ok && ! $check_only; then
        run rustup toolchain install "$channel" --profile minimal \
            --component rustfmt --component clippy
    fi
    if toolchain_ok; then
        have_v=$(rustup run "$channel" rustc --version | awk '{print $2}')
        if ! version_ge "$have_v" "$need" && ! $check_only; then
            run rustup update "$channel"
            have_v=$(rustup run "$channel" rustc --version | awk '{print $2}')
        fi
        if version_ge "$have_v" "$need"; then
            label="Rust $channel $have_v"
            [ "$channel" = "$have_v" ] && label="Rust $have_v"
            ok "$label with rustfmt and clippy (needs $need)"
            rust_ready=true
        else
            bad "Rust $need or newer, have $have_v: run rustup update $channel"
        fi
    else
        bad "Rust $channel toolchain with rustfmt and clippy"
    fi
fi

if $rust_ready && [ "$os" = Linux ]; then
    # The release builds (dist build, CI's release-build job) are static musl.
    musl=$(uname -m)-unknown-linux-musl
    if ! rustup target list --installed --toolchain "$channel" | grep -x "$musl" >/dev/null; then
        $check_only || run rustup target add --toolchain "$channel" "$musl"
    fi
    if rustup target list --installed --toolchain "$channel" | grep -x "$musl" >/dev/null; then
        ok "target $musl"
    else
        warn "target $musl, for release builds"
    fi
fi

# The first x.y.z in the first line a version command prints, or nothing.
tool_version() {
    { $1 2>/dev/null || true; } | sed -n '1s/[^0-9]*\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p'
}

# A cargo-installed tool at the version CI pins.
#   cargo_tool CRATE PINNED-VERSION "VERSION-COMMAND" WHAT-FOR
# A missing tool is installed at the pinned version. A different version is
# left alone: the message says how to match CI.
cargo_tool() {
    local crate=$1 pin=$2 cmd=$3 why=$4 have_v
    if [ -z "$pin" ]; then
        warn "$crate: no pinned version found, skipped"
        return
    fi
    have_v=$(tool_version "$cmd")
    if [ -z "$have_v" ] && $rust_ready && ! $check_only; then
        run cargo install --locked "$crate" --version "$pin"
        have_v=$(tool_version "$cmd")
    fi
    if [ -z "$have_v" ]; then
        warn "$crate $pin, $why"
    elif [ "$have_v" = "$pin" ]; then
        ok "$crate $have_v"
    else
        ok "$crate $have_v (CI uses $pin: cargo install --locked $crate --version $pin)"
    fi
}

nextest_pin=$(sed -n 's/.*cargo-nextest@\([0-9.]*\).*/\1/p' "$root/.github/workflows/ci.yml" | sed -n 1p)
dist_pin=$(sed -n 's/^cargo-dist-version *= *"\(.*\)"/\1/p' "$root/dist-workspace.toml")
mdbook_pin=$(sed -n 's/^ *MDBOOK_VERSION: *\([0-9.]*\).*/\1/p' "$root/.github/workflows/site.yml" | sed -n 1p)
cargo_tool cargo-nextest "$nextest_pin" "cargo nextest --version" "to run the tests as CI does"
cargo_tool cargo-dist "$dist_pin" "dist --version" "for release builds"
cargo_tool mdbook "$mdbook_pin" "mdbook --version" "to build the book and site"

# ---------------------------------------------------------------- System
step "System tools (install these with your package manager)"

if have cc; then ok "C compiler (cc)"; else bad "C compiler (cc): $(hint cc)"; fi
if have git; then ok "git"; else bad "git: $(hint git)"; fi

# GNU rsync 3.1+, for the comparison test. macOS ships 2.6.9 or openrsync.
rsync_v=""
if have rsync; then
    rsync_v=$(rsync --version 2>/dev/null | sed -n '1s/^rsync *version \([0-9.]*\).*/\1/p' || true)
fi
if [ -n "$rsync_v" ] && version_ge "$rsync_v" 3.1; then
    ok "GNU rsync $rsync_v"
else
    bad "GNU rsync 3.1 or newer${rsync_v:+, have $rsync_v}: $(hint rsync)"
fi

# Python 3.11+ (tomllib), for the release, packaging and site scripts.
py_v=""
if have python3; then
    py_v=$(python3 --version 2>&1 | awk '{print $2}')
fi
if [ -n "$py_v" ] && version_ge "$py_v" 3.11; then
    ok "Python $py_v"
else
    bad "Python 3.11 or newer${py_v:+, have $py_v}: $(hint python3)"
fi

if [ "$os" = Linux ]; then
    if have musl-gcc; then ok "musl-gcc"; else warn "musl-gcc, for release builds: $(hint musl-gcc)"; fi
fi
if have node && have npx; then ok "node and npx"; else warn "node and npx, for site/serve.sh and site/deploy.sh: $(hint node)"; fi

if [ "$os" = Linux ]; then
    for t in nfpm apt-ftparchive createrepo_c rpm rpmsign gpg dpkg-deb; do
        if have "$t"; then ok "$t"; else warn "$t, for the packaging tests: $(hint "$t")"; fi
    done
fi

# ---------------------------------------------------------------- Repo
step "This checkout"
hooks=$(git -C "$root" config core.hooksPath 2>/dev/null || true)
if [ "$hooks" != .githooks ] && have git && ! $check_only; then
    run git -C "$root" config core.hooksPath .githooks
    hooks=.githooks
fi
if [ "$hooks" = .githooks ]; then
    ok "git hooks from .githooks (formatting checked on push)"
else
    warn "git hooks: git config core.hooksPath .githooks"
fi

if [ "$failed" = 0 ]; then
    echo "Ready."
else
    echo "Something required is missing; see above."
fi
exit "$failed"
