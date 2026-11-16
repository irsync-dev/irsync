#!/usr/bin/env bash
# Sets up a development machine for this repo.
#
# Usage: tools/dev-setup.sh [--check]
#
# Installs what it can without root: rustup itself, the toolchain from
# rust-toolchain.toml with rustfmt and clippy, the musl targets on Linux,
# and cargo-nextest, cargo-deny, dist, actionlint and mdbook. The CI tools
# are held to the versions CI uses: another version is replaced. (mdbook is
# not: any version builds the book.) Points git at the repo's hooks. Checks
# the rest, which comes from the system package manager, and says which
# package to install (apt, brew, or pkg on FreeBSD). Safe to run again. With
# all of it in place, tools/ci.sh runs the CI checks here.
#
# --check changes nothing; it only reports. The exit status is 1 when
# something required is missing, else 0. Missing optional tools, and ones
# at another version than CI's, only warn.
set -euo pipefail
# With pipefail, never end a pipe in a reader that stops early (head -1,
# grep -q): the writer then dies of SIGPIPE and the pipe fails. Use
# sed -n 1p and grep >/dev/null, which read to the end.

root=$(cd "$(dirname "$0")/.." && pwd -P)
check_only=false
case ${1:-} in
    "") ;;
    --check) check_only=true ;;
    -h | --help) sed -n '2,17p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "usage: $0 [--check]" >&2; exit 2 ;;
esac

os=$(uname -s)
failed=0
ok() { printf '  ok       %s\n' "$*"; }
# warn and bad take what is missing and, optionally, the command that
# installs it. The command gets a line of its own, easy to copy.
fix() { [ -z "$1" ] || printf '               %s\n' "$1"; }
warn() { printf '  missing  %s (optional)\n' "$1"; fix "${2:-}"; }
bad() { printf '  missing  %s (required)\n' "$1"; fix "${2:-}"; failed=1; }
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

# The command that installs a system tool, for the "missing" message.
hint() {
    if [ "$os" = Darwin ]; then
        case $1 in
            cc | git) echo "xcode-select --install" ;;
            python3) echo "brew install python" ;;
            node) echo "brew install node" ;;
            *) echo "brew install $1" ;;
        esac
    elif [ "$os" = FreeBSD ]; then
        case $1 in
            cc) echo "pkg install llvm" ;;
            node) echo "pkg install node npm" ;;
            pandoc) echo "pkg install hs-pandoc" ;;
            *) echo "pkg install $1" ;;
        esac
    else
        case $1 in
            cc) echo "apt install build-essential" ;;
            musl-gcc) echo "apt install musl-tools" ;;
            xz) echo "apt install xz-utils" ;;
            node) echo "apt install nodejs npm" ;;
            apt-ftparchive) echo "apt install apt-utils" ;;
            createrepo_c) echo "apt install createrepo-c" ;;
            rpmsign) echo "apt install rpm" ;;
            gpg) echo "apt install gnupg" ;;
            dpkg-deb) echo "apt install dpkg" ;;
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
    bad "rustup" "tools/dev-setup.sh"
else
    have curl || { bad "curl, to download rustup" "$(hint curl)"; exit 1; }
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
            bad "Rust $need or newer, have $have_v" "rustup update $channel"
        fi
    else
        bad "Rust $channel toolchain with rustfmt and clippy" \
            "rustup toolchain install $channel --profile minimal --component rustfmt --component clippy"
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
        warn "target $musl, for release builds" "rustup target add --toolchain $channel $musl"
    fi
fi

# The first x.y.z in the first line a version command prints, or nothing.
tool_version() {
    { $1 2>/dev/null || true; } | sed -n '1s/[^0-9]*\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p'
}

# A cargo-installed tool at the version CI pins.
#   cargo_tool CRATE PINNED-VERSION "VERSION-COMMAND" WHAT-FOR [any]
# A missing tool is installed at the pinned version, and so is one at
# another version: cargo install replaces it. With "any", whatever version
# is there stays.
cargo_tool() {
    local crate=$1 pin=$2 cmd=$3 why=$4 any=${5:-} have_v
    local install="cargo install --locked $crate"
    [ -n "$any" ] || install="$install --version $pin"
    if [ -z "$pin" ]; then
        warn "$crate: no pinned version found, skipped"
        return
    fi
    have_v=$(tool_version "$cmd")
    if [ -n "$have_v" ] && [ -n "$any" ]; then
        ok "$crate $have_v"
        return
    fi
    if [ "$have_v" != "$pin" ] && $rust_ready && ! $check_only; then
        run $install
        have_v=$(tool_version "$cmd")
    fi
    if [ -z "$have_v" ] && [ -n "$any" ]; then
        warn "$crate, $why" "$install"
    elif [ -z "$have_v" ]; then
        warn "$crate $pin, $why" "$install"
    elif [ "$have_v" = "$pin" ]; then
        ok "$crate $have_v"
    else
        warn "$crate $pin, have $have_v, $why" "$install"
    fi
}

nextest_pin=$(sed -n 's/.*cargo-nextest@\([0-9.]*\).*/\1/p' "$root/.github/workflows/ci.yml" | sed -n 1p)
dist_pin=$(sed -n 's/^cargo-dist-version *= *"\(.*\)"/\1/p' "$root/dist-workspace.toml")
mdbook_pin=$(sed -n 's/^ *MDBOOK_VERSION: *\([0-9.]*\).*/\1/p' "$root/.github/workflows/site.yml" | sed -n 1p)
deny_pin=$(sed -n 's/.*cargo-deny@\([0-9.]*\).*/\1/p' "$root/.github/workflows/ci.yml" | sed -n 1p)
cargo_tool cargo-nextest "$nextest_pin" "cargo nextest --version" "for tools/ci.sh check"
cargo_tool cargo-deny "$deny_pin" "cargo deny --version" "for tools/ci.sh deny"
cargo_tool cargo-dist "$dist_pin" "dist --version" "for release builds and tools/ci.sh release-build"
cargo_tool mdbook "$mdbook_pin" "mdbook --version" "to build the book and site" any

# actionlint, for tools/ci.sh workflows. Not a crate: the release binary is
# downloaded, checked against the SHA-256 below and put next to the cargo
# tools. To move to a new version, change it in the workflows and replace
# these lines from actionlint_VERSION_checksums.txt on the release page;
# tools/tests/test_dev_setup.py holds the two together.
actionlint_pin=1.7.7
actionlint_sha256() {
    case $1 in
        linux_amd64) echo 023070a287cd8cccd71515fedc843f1985bf96c436b7effaecce67290e7e0757 ;;
        linux_arm64) echo 401942f9c24ed71e4fe71b76c7d638f66d8633575c4016efd2977ce7c28317d0 ;;
        darwin_amd64) echo 28e5de5a05fc558474f638323d736d822fff183d2d492f0aecb2b73cc44584f5 ;;
        darwin_arm64) echo 2693315b9093aeacb4ebd91a993fea54fc215057bf0da2659056b4bc033873db ;;
        freebsd_amd64) echo f6eec0e5efd17183a954f0d88280885b7a58f39808050cdfcd7f068fc1734bc8 ;;
    esac
}
sha256_of() {
    # sha256 is FreeBSD's own; -q prints only the digest.
    if have sha256sum; then
        sha256sum "$1"
    elif have shasum; then
        shasum -a 256 "$1"
    else
        sha256 -q "$1"
    fi | awk '{print $1}'
}
# Prints nothing and returns 1 if it cannot install; the caller reports.
install_actionlint() {
    local plat want d got rc=0
    case $(uname -m) in
        x86_64 | amd64) plat=amd64 ;; # FreeBSD says amd64
        aarch64 | arm64) plat=arm64 ;;
        *) return 1 ;;
    esac
    plat=$(echo "$os" | tr '[:upper:]' '[:lower:]')_$plat
    want=$(actionlint_sha256 "$plat")
    [ -n "$want" ] && have curl && have tar || return 1
    d=$(mktemp -d) || return 1
    printf '  running  download actionlint %s (%s)\n' "$actionlint_pin" "$plat"
    if curl --proto '=https' --tlsv1.2 -fsSL -o "$d/a.tar.gz" \
        "https://github.com/rhysd/actionlint/releases/download/v$actionlint_pin/actionlint_${actionlint_pin}_$plat.tar.gz"; then
        got=$(sha256_of "$d/a.tar.gz")
        if [ "$got" = "$want" ]; then
            mkdir -p "$cargo_bin" && tar -xzf "$d/a.tar.gz" -C "$cargo_bin" actionlint || rc=1
        else
            printf '  refused  actionlint download: SHA-256 is %s, expected %s\n' "$got" "$want"
            rc=1
        fi
    else
        rc=1
    fi
    rm -rf "$d"
    return $rc
}
actionlint_v=$(tool_version "actionlint -version")
if [ "$actionlint_v" != "$actionlint_pin" ] && ! $check_only; then
    install_actionlint || true
    actionlint_v=$(tool_version "actionlint -version")
fi
if [ -z "$actionlint_v" ]; then
    warn "actionlint $actionlint_pin, for tools/ci.sh workflows" "tools/dev-setup.sh"
elif [ "$actionlint_v" = "$actionlint_pin" ]; then
    ok "actionlint $actionlint_v"
else
    warn "actionlint $actionlint_pin, have $actionlint_v, for tools/ci.sh workflows" "tools/dev-setup.sh"
fi

# ---------------------------------------------------------------- System
step "System tools (install these with your package manager)"

if have cc; then ok "C compiler (cc)"; else bad "C compiler (cc)" "$(hint cc)"; fi
if have git; then ok "git"; else bad "git" "$(hint git)"; fi

# GNU rsync 3.1+, for the comparison test. macOS ships 2.6.9 or openrsync.
rsync_v=""
if have rsync; then
    rsync_v=$(rsync --version 2>/dev/null | sed -n '1s/^rsync *version \([0-9.]*\).*/\1/p' || true)
fi
if [ -n "$rsync_v" ] && version_ge "$rsync_v" 3.1; then
    ok "GNU rsync $rsync_v"
else
    bad "GNU rsync 3.1 or newer${rsync_v:+, have $rsync_v}" "$(hint rsync)"
fi

# pandoc and mandoc build and check the man pages (tools/build-man.sh).
if have pandoc; then ok "pandoc"; else warn "pandoc, for the man pages" "$(hint pandoc)"; fi
if have mandoc; then ok "mandoc"; else warn "mandoc, for the man pages" "$(hint mandoc)"; fi

# Python 3.11+ (tomllib), for the release, packaging and site scripts.
py_v=""
if have python3; then
    py_v=$(python3 --version 2>&1 | awk '{print $2}')
fi
if [ -n "$py_v" ] && version_ge "$py_v" 3.11; then
    ok "Python $py_v"
else
    bad "Python 3.11 or newer${py_v:+, have $py_v}" "$(hint python3)"
fi

# The workflow rule tests (tools/ci.sh workflows) read YAML.
if have python3 && python3 -c 'import yaml' >/dev/null 2>&1; then
    ok "PyYAML"
elif [ "$os" = Darwin ]; then
    warn "PyYAML, for tools/ci.sh workflows" "python3 -m pip install pyyaml"
elif [ "$os" = FreeBSD ]; then
    # The package is named after the Python it is for: py311-pyyaml.
    py_pkg=$(echo "${py_v:-3.11}" | awk -F. '{print "py" $1 $2 "-pyyaml"}')
    warn "PyYAML, for tools/ci.sh workflows" "pkg install $py_pkg"
else
    warn "PyYAML, for tools/ci.sh workflows" "apt install python3-yaml"
fi

# curl, and a tar that writes and reads .tar.xz, for the tests of the
# release download (crates/irsync/tests/ssh_release.rs). GNU tar needs the
# xz program for that; macOS's tar has xz built in.
if have curl; then ok "curl"; else bad "curl" "$(hint curl)"; fi
tar_xz_ok() {
    local d rc=0
    d=$(mktemp -d) || return 1
    echo x >"$d/f"
    { tar -cJf "$d/t.tar.xz" -C "$d" f && tar -tJf "$d/t.tar.xz" >/dev/null; } 2>/dev/null || rc=1
    rm -rf "$d"
    return $rc
}
if ! have tar; then
    bad "tar" "$(hint tar)"
elif tar_xz_ok; then
    ok "tar with xz"
else
    bad "xz, so tar can write and read .tar.xz" "$(hint xz)"
fi

if [ "$os" = Linux ]; then
    if have musl-gcc; then ok "musl-gcc"; else warn "musl-gcc, for release builds and tools/ci.sh release-build" "$(hint musl-gcc)"; fi
fi
if have node && have npx; then ok "node and npx"; else warn "node and npx, for site/build.sh, site/serve.sh and site/deploy.sh" "$(hint node)"; fi

if [ "$os" = Linux ]; then
    # nfpm at the version CI installs (.github/scripts/install-packaging-tools.sh).
    nfpm_pin=$(sed -n 's/^NFPM_VERSION=\([0-9.]*\)$/\1/p' "$root/.github/scripts/install-packaging-tools.sh")
    nfpm_install="go install github.com/goreleaser/nfpm/v2/cmd/nfpm@${nfpm_pin:+v}${nfpm_pin:-latest}"
    nfpm_v=$(tool_version "nfpm --version")
    if ! have nfpm; then
        warn "nfpm${nfpm_pin:+ $nfpm_pin}, for the packaging tests" "$nfpm_install"
    elif [ -z "$nfpm_v" ] || [ -z "$nfpm_pin" ] || [ "$nfpm_v" = "$nfpm_pin" ]; then
        # A build that does not print its version is taken as it is.
        ok "nfpm${nfpm_v:+ $nfpm_v}"
    else
        warn "nfpm $nfpm_pin, have $nfpm_v, for the packaging tests" "$nfpm_install"
    fi
    for t in apt-ftparchive createrepo_c rpm rpmsign gpg dpkg-deb; do
        if have "$t"; then ok "$t"; else warn "$t, for the packaging tests" "$(hint "$t")"; fi
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
    warn "git hooks" "git config core.hooksPath .githooks"
fi

if [ "$failed" = 0 ]; then
    echo "Ready."
else
    echo "Something required is missing; see above."
fi
exit "$failed"
