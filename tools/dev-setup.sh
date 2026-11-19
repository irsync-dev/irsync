#!/usr/bin/env bash
# Sets up a development machine for this repo.
#
# Usage: tools/dev-setup.sh [--check]
#
# Installs what it can without root: rustup itself, the toolchain from
# rust-toolchain.toml with rustfmt and clippy, the musl targets on Linux,
# and cargo-nextest, cargo-deny, dist, actionlint and mdbook. Those five are
# downloaded from their release pages and checked against SHA-256s kept
# here; cargo builds one only where it has no binary for the machine, or the
# hashes here are for another version than CI's. They are held to the
# versions CI uses: another version is replaced. Points git at the repo's
# hooks. Checks the rest, which comes from the system package manager, and
# says which package to install (apt, brew, or pkg on FreeBSD). Safe to run
# again. With all of it in place, tools/ci.sh runs the CI checks here.
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
    -h | --help) sed -n '2,19p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
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
            cc) echo "sudo pkg install llvm" ;;
            node) echo "sudo pkg install node npm" ;;
            pandoc) echo "sudo pkg install hs-pandoc" ;;
            shellcheck) echo "sudo pkg install hs-ShellCheck" ;;
            *) echo "sudo pkg install $1" ;;
        esac
    else
        case $1 in
            cc) echo "sudo apt install build-essential" ;;
            musl-gcc) echo "sudo apt install musl-tools" ;;
            xz) echo "sudo apt install xz-utils" ;;
            node) echo "sudo apt install nodejs npm" ;;
            apt-ftparchive) echo "sudo apt install apt-utils" ;;
            createrepo_c) echo "sudo apt install createrepo-c" ;;
            rpmsign) echo "sudo apt install rpm" ;;
            gpg) echo "sudo apt install gnupg" ;;
            dpkg-deb) echo "sudo apt install dpkg" ;;
            *) echo "sudo apt install $1" ;;
        esac
    fi
}

have() { command -v "$1" >/dev/null 2>&1; }

# ---------------------------------------------------------------- Rust
step "Rust (rustup, and tools from their release pages)"

# rustup and cargo install into $CARGO_HOME/bin, ~/.cargo/bin by default.
cargo_bin=${CARGO_HOME:-$HOME/.cargo}/bin
# rustup is there, but PATH does not lead to it. The script goes on with
# it; the shell that started the script will not find it, nor the tools.
rustup_hidden=false
if ! have rustup && [ -x "$cargo_bin/rustup" ]; then
    PATH="$cargo_bin:$PATH"
    rustup_hidden=true
fi
if $rustup_hidden; then
    bad "rustup on PATH; it is in $cargo_bin, with the tools below" "add $cargo_bin to PATH"
elif have rustup; then
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
    { $1 2>/dev/null || true; } | first_version
}
first_version() {
    sed -n '1s/[^0-9]*\([0-9][0-9]*\.[0-9][0-9]*\.[0-9][0-9]*\).*/\1/p'
}
# The same for a program in $cargo_bin, whatever PATH finds.
#   version_in_cargo_bin PROGRAM ARGUMENT...
version_in_cargo_bin() {
    local bin=$cargo_bin/$1
    shift
    { "$bin" "$@" 2>/dev/null || true; } | first_version
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

# The directory a download is unpacked in. It goes away when the script
# ends, also when it is stopped half way.
fetch_dir=""
trap '[ -z "$fetch_dir" ] || rm -rf "$fetch_dir"' EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

# fetch_tool NAME VERSION PLAT URL SHA256 MEMBER
# Downloads URL, checks it against SHA256 and puts MEMBER (a path inside the
# archive, .tar.gz or .tar.xz) into $cargo_bin under its base name. Nothing
# else from the archive lands there. Says what it downloads, and why when
# it fails or refuses a download with another hash.
# Returns 0 installed, 1 could not download or unpack, 2 refused.
fetch_tool() {
    local name=$1 version=$2 plat=$3 url=$4 want=$5 member=$6 d got hint="" rc=0
    fetch_failed() { printf '  failed   %s download: %s\n' "$name" "$1"; }
    if ! have curl || ! have tar; then
        fetch_failed "it needs curl and tar"
        return 1
    fi
    if ! have sha256sum && ! have shasum && ! have sha256; then
        fetch_failed "there is no sha256sum, shasum or sha256 to check it with"
        return 1
    fi
    fetch_dir=$(mktemp -d) || return 1
    d=$fetch_dir
    printf '  running  download %s %s (%s)\n' "$name" "$version" "$plat"
    if curl --proto '=https' --tlsv1.2 -fsSL -o "$d/archive" "$url"; then
        got=$(sha256_of "$d/archive")
        if [ "$got" != "$want" ]; then
            printf '  refused  %s download: SHA-256 is %s, expected %s\n' "$name" "$got" "$want"
            rc=2
        # tar finds out by itself whether it is gzip or xz.
        elif ! { mkdir -p "$d/x" "$cargo_bin" &&
            tar -xf "$d/archive" -C "$d/x" "$member" &&
            mv -f "$d/x/$member" "$cargo_bin/${member##*/}"; }; then
            # GNU tar runs xz as a program.
            case $url in *.tar.xz) have xz || hint="; is xz installed?" ;; esac
            fetch_failed "could not unpack it into $cargo_bin$hint"
            rc=1
        fi
    else
        fetch_failed "could not fetch $url"
        rc=1
    fi
    rm -rf "$d"
    fetch_dir=""
    return $rc
}

# The Rust target whose release binaries run here, or nothing. Linux takes
# the static musl builds, so the host's libc does not matter.
tool_target() {
    case $os:$(uname -m) in
        Linux:x86_64) echo x86_64-unknown-linux-musl ;;
        Linux:aarch64 | Linux:arm64) echo aarch64-unknown-linux-musl ;;
        Darwin:x86_64) echo x86_64-apple-darwin ;;
        Darwin:arm64) echo aarch64-apple-darwin ;;
        FreeBSD:amd64 | FreeBSD:x86_64) echo x86_64-unknown-freebsd ;;
    esac
}

# The tools below are downloaded as release binaries and checked against
# these hashes. To move one to a new version: change the pin where it lives
# (see the *_pin lines below), change its version here, and replace its
# hash lines. tools/tests/test_dev_setup.py holds the pin and the version
# here together; when they differ, the tool is compiled instead.

# The version the hashes below are for.
tool_hashes_for() {
    case $1 in
        cargo-nextest) echo 0.9.151 ;;
        cargo-deny) echo 0.20.2 ;;
        cargo-dist) echo 0.34.0 ;;
        mdbook) echo 0.5.2 ;;
    esac
}
# The SHA-256 of a tool's release archive for a target, or nothing where
# there is none. nextest, deny and dist publish a .sha256 file next to each
# archive. mdbook publishes none: download its archives and hash them.
tool_sha256() {
    case $1:$2 in
        cargo-nextest:x86_64-unknown-linux-musl) echo 6b284d6fde9670dfb6ee10ea9d9abbc85f95643d3f592472d420d82e257b3a6c ;;
        cargo-nextest:aarch64-unknown-linux-musl) echo a9ee2d53cf0711d879f068c0ff814b6fe3af3f9be8799f8ab484e47571087160 ;;
        # One universal archive for both Macs.
        cargo-nextest:x86_64-apple-darwin | cargo-nextest:aarch64-apple-darwin) echo d1d7d025d4bfc9fb154cd76aea3949bd78534829a0589abefa6e5dd4edb135dd ;;
        cargo-nextest:x86_64-unknown-freebsd) echo a353c1c6a4488b2be2c3c584852b18caa6830b5fa2ccdc0349e3971418b01d86 ;;
        cargo-deny:x86_64-unknown-linux-musl) echo 9f12ed4c49936e09b48bf862b595cde2fe64fcbd9d74dfacac6131ca824c8d5f ;;
        cargo-deny:aarch64-unknown-linux-musl) echo 995c82be0defc7a025cae49a2aa2644ce8245c9a3318fc4103907c6a285e8c7d ;;
        cargo-deny:x86_64-apple-darwin) echo 248da7f581724e470071990c088ffc55c811981715f4cbdb258621fb79f8b7a6 ;;
        cargo-deny:aarch64-apple-darwin) echo fe67d82a10d8597a3549364cb733a3f9cc1bfff9031b7ae46384a9f2a72090c3 ;;
        cargo-dist:x86_64-unknown-linux-musl) echo f274b9889e93333a52cdcab86b93ab04b875b8ef4eebc7f8589514e0aff827e6 ;;
        cargo-dist:aarch64-unknown-linux-musl) echo aac7d48901f5283b53e136dd0498782c88a3350dfa4a5e88f4c3e0ba7e486eea ;;
        cargo-dist:x86_64-apple-darwin) echo 6aa89bb1e7c18f51ae8b148ce2fc6ab00ac46ed8173d4f2ed9afa12101218773 ;;
        cargo-dist:aarch64-apple-darwin) echo d1eec30e63eed23e5a5bd48fe96eae314799811bffca254ec1d2264638ba1890 ;;
        mdbook:x86_64-unknown-linux-musl) echo 8e9d802bbecea34380aabfc65bec4fdba23c07453ef2c7e06fa34c282082e2c4 ;;
        mdbook:aarch64-unknown-linux-musl) echo fb229bfdc68dd6c93692e64c502a67a8d3d2fc35c37c61fc72768843e6476add ;;
        mdbook:x86_64-apple-darwin) echo 17cc64478ec279a73881420e850bd8f9d460552e56b50159ff465bc97eb90d6c ;;
        mdbook:aarch64-apple-darwin) echo da2f55653e96e3f6e1c53e2e13e91cc0cfbce8ab971c2e0de792c0f1f8d24222 ;;
    esac
}

# tool_archive CRATE VERSION TARGET
# Prints the URL of the release archive and, after a space, the path of the
# binary inside it.
tool_archive() {
    local v=$2 t=$3
    case $1 in
        cargo-nextest)
            case $t in *-apple-darwin) t=universal-apple-darwin ;; esac
            echo "https://github.com/nextest-rs/nextest/releases/download/cargo-nextest-$v/cargo-nextest-$v-$t.tar.gz cargo-nextest"
            ;;
        cargo-deny) echo "https://github.com/EmbarkStudios/cargo-deny/releases/download/$v/cargo-deny-$v-$t.tar.gz cargo-deny-$v-$t/cargo-deny" ;;
        cargo-dist) echo "https://github.com/axodotdev/cargo-dist/releases/download/v$v/cargo-dist-$t.tar.xz cargo-dist-$t/dist" ;;
        mdbook) echo "https://github.com/rust-lang/mdBook/releases/download/v$v/mdbook-v$v-$t.tar.gz mdbook" ;;
    esac
}

# A tool at the version CI pins.
#   cargo_tool CRATE PINNED-VERSION "VERSION-COMMAND" WHAT-FOR
# A missing tool is installed at the pinned version, and so is one at
# another version: it is replaced. The release binary is downloaded where
# there is one for this machine. A download that fails or is refused is
# reported; it is never made up for by compiling. Only where there is no
# binary, or the hashes above are for another version, cargo builds it. A
# build that fails is reported, and the script goes on.
cargo_tool() {
    local crate=$1 pin=$2 cmd=$3 why=$4 have_v target want="" for_v archive bin there_v fetched=false
    if [ -z "$pin" ]; then
        warn "$crate: no pinned version found, skipped"
        return
    fi
    # --force: cargo will not replace a binary it did not install itself.
    local compile="cargo install --locked $crate --version $pin --force"
    local fix=$compile
    target=$(tool_target)
    for_v=$(tool_hashes_for "$crate")
    [ -z "$target" ] || want=$(tool_sha256 "$crate" "$target")
    have_v=$(tool_version "$cmd")
    if [ -n "$want" ] && [ "$for_v" != "$pin" ]; then
        if [ "$have_v" != "$pin" ] && ! $check_only; then
            if $rust_ready; then
                printf '  note     %s: the hashes are for %s, CI uses %s; compiling\n' "$crate" "$for_v" "$pin"
            else
                printf '  note     %s: the hashes are for %s, CI uses %s; cargo has to build it, and Rust is not ready\n' "$crate" "$for_v" "$pin"
            fi
        fi
        want=""
    fi
    [ -z "$want" ] || fix="tools/dev-setup.sh"
    if [ "$have_v" != "$pin" ] && ! $check_only; then
        if [ -n "$want" ]; then
            archive=$(tool_archive "$crate" "$pin" "$target")
            bin=${archive#* }
            bin=${bin##*/}
            case $crate in
                cargo-nextest | cargo-deny) there_v=$(version_in_cargo_bin "$bin" "${crate#cargo-}" --version) ;;
                *) there_v=$(version_in_cargo_bin "$bin" --version) ;;
            esac
            # Already downloaded, but PATH does not lead to it: say so below,
            # do not download it again.
            if [ "$there_v" = "$pin" ] ||
                fetch_tool "$crate" "$pin" "$target" "${archive% *}" "$want" "${archive#* }"; then
                fetched=true
            fi
        elif $rust_ready; then
            run $compile || true
        fi
        have_v=$(tool_version "$cmd")
    fi
    if [ "$have_v" = "$pin" ]; then
        ok "$crate $have_v"
    elif $fetched && [ -z "$have_v" ]; then
        warn "$crate $pin, $why; it is in $cargo_bin" "add $cargo_bin to PATH"
    elif $fetched; then
        # Another copy comes first on PATH.
        warn "$crate $pin, have $have_v, $why; $pin is in $cargo_bin" "put $cargo_bin first in PATH"
    elif [ -z "$have_v" ]; then
        warn "$crate $pin, $why" "$fix"
    else
        warn "$crate $pin, have $have_v, $why" "$fix"
    fi
}

nextest_pin=$(sed -n 's/.*cargo-nextest@\([0-9.]*\).*/\1/p' "$root/.github/workflows/ci.yml" | sed -n 1p)
dist_pin=$(sed -n 's/^cargo-dist-version *= *"\(.*\)"/\1/p' "$root/dist-workspace.toml")
mdbook_pin=$(sed -n 's/^ *MDBOOK_VERSION: *\([0-9.]*\).*/\1/p' "$root/.github/workflows/site.yml" | sed -n 1p)
deny_pin=$(sed -n 's/.*cargo-deny@\([0-9.]*\).*/\1/p' "$root/.github/workflows/ci.yml" | sed -n 1p)
cargo_tool cargo-nextest "$nextest_pin" "cargo nextest --version" "for tools/ci.sh check"
cargo_tool cargo-deny "$deny_pin" "cargo deny --version" "for tools/ci.sh deny"
cargo_tool cargo-dist "$dist_pin" "dist --version" "for release builds and tools/ci.sh release-build"
cargo_tool mdbook "$mdbook_pin" "mdbook --version" "to build the book and site"

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
# Returns 1 if it cannot install; the caller reports.
install_actionlint() {
    local plat want
    case $(uname -m) in
        x86_64 | amd64) plat=amd64 ;; # FreeBSD says amd64
        aarch64 | arm64) plat=arm64 ;;
        *) return 1 ;;
    esac
    plat=$(echo "$os" | tr '[:upper:]' '[:lower:]')_$plat
    want=$(actionlint_sha256 "$plat")
    [ -n "$want" ] || return 1
    fetch_tool actionlint "$actionlint_pin" "$plat" \
        "https://github.com/rhysd/actionlint/releases/download/v$actionlint_pin/actionlint_${actionlint_pin}_$plat.tar.gz" \
        "$want" actionlint || return 1
}
actionlint_v=$(tool_version "actionlint -version")
actionlint_fetched=false
if [ "$actionlint_v" != "$actionlint_pin" ] && ! $check_only; then
    # As for the cargo tools: not again when it is there and PATH hides it.
    if [ "$(version_in_cargo_bin actionlint -version)" = "$actionlint_pin" ] || install_actionlint; then
        actionlint_fetched=true
    fi
    actionlint_v=$(tool_version "actionlint -version")
fi
if [ "$actionlint_v" = "$actionlint_pin" ]; then
    ok "actionlint $actionlint_v"
elif $actionlint_fetched && [ -z "$actionlint_v" ]; then
    warn "actionlint $actionlint_pin, for tools/ci.sh workflows; it is in $cargo_bin" "add $cargo_bin to PATH"
elif $actionlint_fetched; then
    warn "actionlint $actionlint_pin, have $actionlint_v, for tools/ci.sh workflows; $actionlint_pin is in $cargo_bin" "put $cargo_bin first in PATH"
elif [ -z "$actionlint_v" ]; then
    warn "actionlint $actionlint_pin, for tools/ci.sh workflows" "tools/dev-setup.sh"
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
# tools/ci.sh check stops without them.
if have pandoc; then ok "pandoc"; else bad "pandoc, for the man pages" "$(hint pandoc)"; fi
if have mandoc; then ok "mandoc"; else bad "mandoc, for the man pages" "$(hint mandoc)"; fi

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
    warn "PyYAML, for tools/ci.sh workflows" "sudo pkg install $py_pkg"
else
    warn "PyYAML, for tools/ci.sh workflows" "sudo apt install python3-yaml"
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

# minisign signs releases (.github/scripts/sign-release.sh) and makes the
# signature test files. 0.10 is the first to hash the file before signing by
# default; irsync refuses the older format. It prints "minisign 0.11", with
# two numbers or three.
minisign_v=""
if have minisign; then
    minisign_v=$({ minisign -v 2>/dev/null || true; } | sed -n '1s/^minisign *\([0-9][0-9.]*\).*/\1/p')
fi
if [ -n "$minisign_v" ] && version_ge "$minisign_v" 0.10; then
    ok "minisign $minisign_v"
else
    warn "minisign 0.10 or newer${minisign_v:+, have $minisign_v}, to sign releases" "$(hint minisign)"
fi

# The shellcheck tool finds quoting and other bugs in shell scripts. It prints
# its version on the second line, "version: 0.11.0".
if have shellcheck; then
    ok "shellcheck $({ shellcheck --version 2>/dev/null || true; } | sed -n 's/^version: *//p')"
else
    warn "shellcheck, for tools/ci.sh shell-scripts" "$(hint shellcheck)"
fi

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
