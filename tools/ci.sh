#!/usr/bin/env bash
# Runs the CI checks, on GitHub and on your machine.
#
# Usage: tools/ci.sh [JOB...]
#
# .github/workflows/ci.yml runs this script, one job per name below, so a
# local run and CI run the same commands.
#
#   check          formatting, the dev-setup and man page tests, clippy
#                  with and without OpenTelemetry, every test including
#                  the slow ones, and the doc tests. On Windows (Git Bash)
#                  it covers filegen, fileset and irsync-glob only, the
#                  crates that build there: formatting, one clippy run, their
#                  tests and doc tests, then a smoke run of filegen and
#                  fileset together. It needs only cargo-nextest; the Python
#                  tests and the fuzz target formatting are
#                  left out; the fuzz lock check runs. With no job, only
#                  check runs on Windows.
#   deny           the dependency check (cargo deny, see deny.toml), for
#                  the platforms irsync is built for
#   deny-license-check
#                  the license check for every platform's dependencies
#                  (deny-license-check.toml); on GitHub it blocks a
#                  release, not a pull request
#   workflows      the workflow rule tests, actionlint and zizmor
#   shell-scripts  shellcheck on every shell script in the repository
#   release-build  the release build for this machine's Linux musl target,
#                  dist's global build, and our installer run on the result
#   freebsd-build  the FreeBSD tarball; FreeBSD x86_64 only
#   fuzz-build     builds the fuzz targets in fuzz/ and runs each on its
#                  seeds (fuzz/seeds), with the nightly in
#                  fuzz/rust-toolchain.toml
#   fuzz           runs each fuzz target for FUZZ_SECONDS; only when named
#   miri           the tests of the crates in miri_packages under Miri, which
#                  catches undefined behaviour, with the nightly in
#                  fuzz/rust-toolchain.toml; slow, so only when named
#   typecheck      clippy for every platform in tools/typecheck-targets, from
#                  this machine (tools/typecheck-target.sh): finds code that
#                  does not compile there before CI does; only when named
#
# With no job it runs all of them but fuzz, miri and typecheck, the quick
# ones first.
# fuzz-build and release-build run on Linux only, freebsd-build on FreeBSD
# only; a run of everything leaves each out elsewhere.
#
# tools/dev-setup.sh installs or names the tools this needs. Two things only
# GitHub does: the macOS and arm64 runs, and the dependency review of a pull
# request.
#
# Environment:
#   CARGO_BUILD_TARGET  check builds and tests for this target (CI sets it
#                       for the musl runs of a release)
#   CI_RELEASE_TARGET   release-build builds this target instead of
#                       $(uname -m)-unknown-linux-musl
#   FUZZ_SECONDS        how long fuzz runs each target (default 60)
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$root"
os=$(uname -s)
# Git Bash, MSYS2 and Cygwin name their Windows version, e.g.
# MINGW64_NT-10.0-26100.
case $os in
    MINGW* | MSYS* | CYGWIN*) os=Windows ;;
esac
# The crates that build and test on Windows; check covers only these there.
windows_packages=(filegen fileset irsync-glob)

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; }

all_jobs="workflows shell-scripts deny deny-license-check check fuzz-build release-build"
jobs=""
for arg in "$@"; do
    case $arg in
        -h | --help) usage; exit 0 ;;
        check | deny | deny-license-check | workflows | shell-scripts | fuzz-build | fuzz | miri | release-build | freebsd-build | typecheck) jobs="$jobs $arg" ;;
        *) echo "ci.sh: unknown job '$arg' (jobs: $all_jobs freebsd-build fuzz miri typecheck)" >&2; exit 2 ;;
    esac
done
if [ -z "$jobs" ]; then
    jobs=$all_jobs
    if [ "$os" = Windows ]; then
        jobs="check"
        echo "ci.sh: on Windows only check runs, for filegen, fileset and irsync-glob"
    elif [ "$os" = FreeBSD ]; then
        jobs="workflows shell-scripts deny deny-license-check check freebsd-build"
        echo "ci.sh: leaving out fuzz-build and release-build, which run on Linux only"
    elif [ "$os" != Linux ]; then
        jobs="workflows shell-scripts deny deny-license-check check"
        echo "ci.sh: leaving out fuzz-build and release-build, which run on Linux only"
    fi
fi

# The nightly the fuzz targets and Miri use: the one pin in the repository.
nightly=$(sed -n 's/^channel = "\(.*\)"/\1/p' fuzz/rust-toolchain.toml)

# Prints a command, then runs it.
run() {
    printf '\n== %s\n' "$*"
    "$@"
}
# The same, without the command's own output.
run_quiet() {
    printf '\n== %s\n' "$*"
    "$@" >/dev/null
}

missing=0
# need NAME COMMAND...: the command succeeds if the tool is there.
need() {
    local name=$1
    shift
    if ! "$@" >/dev/null 2>&1; then
        echo "ci.sh: $name not found; tools/dev-setup.sh installs it or says how" >&2
        missing=1
    fi
}

needs_check() {
    need cargo-nextest cargo nextest --version
    # The rest is for the Python and rsync tests, which Windows does not run.
    if [ "$os" = Windows ]; then
        return
    fi
    need rsync rsync --version
    need python3 python3 --version
    need pandoc pandoc --version
    # Presence only: it reads an empty file and succeeds.
    need mandoc mandoc -T ascii /dev/null
}
needs_deny() { need cargo-deny cargo deny --version; }
needs_deny_license_check() { needs_deny; }
needs_workflows() {
    need "PyYAML (the Python yaml module)" python3 -c "import yaml"
    need actionlint actionlint -version
    need zizmor zizmor --version
}
needs_shell_scripts() { need shellcheck shellcheck --version; }
needs_fuzz_build() {
    need cargo-fuzz cargo fuzz --version
    need "Rust $nightly (fuzz/rust-toolchain.toml)" rustup run "$nightly" rustc --version
}
needs_fuzz() { needs_fuzz_build; }
needs_miri() {
    need "Rust $nightly (fuzz/rust-toolchain.toml)" rustup run "$nightly" rustc --version
    need Miri cargo "+$nightly" miri --version
}
# The lines of tools/typecheck-targets, without comments: TARGET CRATE...
typecheck_lines() {
    sed 's/#.*//' tools/typecheck-targets | awk 'NF'
}
needs_typecheck() {
    local installed t
    installed=$(rustup target list --installed 2>/dev/null) || installed=
    for t in $(typecheck_lines | awk '{ print $1 }'); do
        if ! printf '%s\n' "$installed" | awk -v t="$t" '$0 == t { found = 1 } END { exit !found }'; then
            echo "ci.sh: Rust target $t not installed; tools/dev-setup.sh installs it" >&2
            missing=1
        fi
    done
}
needs_release_build() {
    if [ "$os" != Linux ]; then
        echo "ci.sh: release-build runs on Linux only" >&2
        missing=1
        return
    fi
    need "dist (cargo-dist)" dist --version
    need musl-gcc musl-gcc --version
    need pandoc pandoc --version
    need man command -v man
    need mandoc mandoc -T ascii /dev/null
    need groff groff --version
}
needs_freebsd_build() {
    # FreeBSD calls x86_64 amd64.
    if [ "$os" != FreeBSD ] || [ "$(uname -m)" != amd64 ]; then
        echo "ci.sh: freebsd-build runs on FreeBSD x86_64 only" >&2
        missing=1
        return
    fi
    need pandoc pandoc --version
}

smoke_dir=

# Removes the smoke run's directory. On Windows filegen makes names that Git
# Bash's rm cannot open (unpaired surrogates, NUL), so PowerShell removes what
# rm leaves, using the extended-length form of the path. A failure here
# must not fail a check that has passed.
# shellcheck disable=SC2317 # only a trap calls it; some versions call that unreachable
remove_smoke_dir() {
    [ -n "$smoke_dir" ] || return 0
    if ! rm -rf "$smoke_dir" 2>/dev/null && [ "$os" = Windows ]; then
        # shellcheck disable=SC2016 # $env: is PowerShell's, not the shell's
        SMOKE_DIR=$(cygpath -w "$smoke_dir") powershell -NoProfile -NonInteractive -Command \
            '$b = [string][char]92; Remove-Item -LiteralPath ($b + $b + "?" + $b + $env:SMOKE_DIR) -Recurse -Force' >/dev/null 2>&1 || true
    fi
    return 0
}

# A short run of filegen and fileset together, on a tree filegen builds: each
# tool's own tests stop at the other's edge. fileset must take the tree
# filegen leaves, also after filegen changes it again. The tree and filegen's
# output directory are beside each other in a temporary directory, removed on
# exit.
smoke_tools() {
    smoke_dir=$(mktemp -d)
    trap remove_smoke_dir EXIT
    mkdir "$smoke_dir/tree" "$smoke_dir/out"
    printf '\n== smoke run of filegen and fileset in %s\n' "$smoke_dir"
    run cargo run --locked -q -p filegen -- run --seed 1 --steps 300 --quiet \
        --out-dir "$smoke_dir/out" "$smoke_dir/tree"
    run cargo run --locked -q -p fileset -- init --quiet "$smoke_dir/tree"
    run cargo run --locked -q -p fileset -- check --quiet "$smoke_dir/tree"
    run cargo run --locked -q -p filegen -- run --resume "$smoke_dir/out/filegen-1.state.yaml" \
        --steps 100 --quiet "$smoke_dir/tree"
    # update exits 0 unless --fail-on-warnings is given, so 0 is required.
    run cargo run --locked -q -p fileset -- update --quiet "$smoke_dir/tree"
    run cargo run --locked -q -p fileset -- check --quiet "$smoke_dir/tree"
}

job_check() {
    # ${target_args[@]+...} below: bash 3.2 on macOS calls an empty array unset.
    local target_args=() profile=ci scope=(--workspace) p
    if [ -n "${CARGO_BUILD_TARGET:-}" ]; then
        target_args=(--target "$CARGO_BUILD_TARGET")
    fi
    # GitHub's macOS runners are slow, so there the ci-macos profile leaves
    # out slow tests that Linux covers (.config/nextest.toml). A Mac of your
    # own runs them all.
    if [ "${GITHUB_ACTIONS:-}" = true ] && [ "$os" = Darwin ]; then
        profile=ci-macos
    fi
    # Only the crates that build on Windows, by name instead of --workspace.
    if [ "$os" = Windows ]; then
        scope=()
        for p in "${windows_packages[@]}"; do
            scope+=(-p "$p")
        done
    else
        # Re-runs the build script, which bakes in the git commit; a test
        # compares the two. A fresh checkout does not need it, a used one does.
        touch crates/irsync/build.rs
    fi

    run cargo fmt --all --check
    if [ "$os" != Windows ]; then
        # The fuzz targets are outside the workspace, so cargo fmt skips them.
        run rustfmt --edition 2021 --check fuzz/fuzz_targets/*.rs
    fi
    # fuzz/Cargo.lock follows Cargo.lock. Here and not only in fuzz-build,
    # which needs a nightly and runs on Linux only.
    run tools/sync-fuzz-lock.sh --check
    if [ "$os" != Windows ]; then
        # Tests tools/dev-setup.sh and tools/build-man.sh. They are Python, so no
        # cargo command runs them. A missing pandoc or mandoc fails here instead
        # of skipping the man page tests.
        MAN_TESTS_REQUIRED=1 run python3 -m unittest discover -s tools/tests
    fi
    run cargo clippy --locked "${scope[@]}" --all-targets --all-features ${target_args[@]+"${target_args[@]}"} -- -D warnings
    if [ "$os" != Windows ]; then
        # Keeps the build without the otel feature compiling.
        run cargo clippy --locked "${scope[@]}" --all-targets --no-default-features ${target_args[@]+"${target_args[@]}"} -- -D warnings
    fi
    # nextest runs each test in its own process, tests from all files side
    # by side, and kills a hung test (see .config/nextest.toml).
    # --run-ignored all adds the slow netstress tests.
    run cargo nextest run --locked "${scope[@]}" --all-features --profile "$profile" \
        --run-ignored all ${target_args[@]+"${target_args[@]}"}
    # nextest does not run doc tests.
    run cargo test --locked "${scope[@]}" --all-features --doc ${target_args[@]+"${target_args[@]}"}
    if [ "$os" = Windows ]; then
        smoke_tools
    fi
}

# Known vulnerabilities, unmaintained or yanked crates, licenses, sources and
# duplicate crate versions, as set in deny.toml, for the platforms irsync is
# built for. An exception in deny.toml that no longer matches anything fails
# too, so it gets removed.
job_deny() {
    run cargo deny --locked check -D advisory-not-detected -D unmatched-skip -D unnecessary-skip
}

# The licenses of every crate in Cargo.lock, whatever platform it is built
# for.
job_deny_license_check() {
    run cargo deny --locked --config deny-license-check.toml check licenses
}

# The workflow rules, including the supply chain policy in
# .github/scripts/tests/test_workflow_policy.py, then actionlint and
# zizmor, which looks for security problems. zizmor reads the findings to
# ignore, each with its reason, from .github/zizmor.yml. --offline: no
# GitHub API calls, so no token is needed and a run here and on GitHub
# finds the same.
job_workflows() {
    run python3 -m unittest discover -s .github/scripts/tests
    # actionlint's own shellcheck run is off: it would also lint the
    # generated release.yml.
    run actionlint -shellcheck= -pyflakes=
    run zizmor --offline .github/workflows
}

# Every tracked shell script: the ones named *.sh, and any other file whose
# first line starts sh, bash or dash (the git hooks, test fixtures, the
# installer template). A deliberate exception is turned off on its own line,
# with the reason next to it.
job_shell_scripts() {
    local files=() file first
    # Kept in a variable: bash 3.2 on macOS reads a quoted regex literally.
    local shebang='^#!.*[/ ](ba|da)?sh( |$)'
    while IFS= read -r -d '' file; do
        case $file in
            *.sh) files+=("$file") ;;
            *)
                [ -f "$file" ] && [ ! -L "$file" ] || continue
                first=$(head -n 1 "$file" 2>/dev/null | tr -d '\0') || true
                if [[ $first =~ $shebang ]]; then
                    files+=("$file")
                fi
                ;;
        esac
    done < <(git ls-files -z)
    run shellcheck "${files[@]}"
}

# The fuzz targets: fuzz/fuzz_targets/NAME.rs, with its seeds in
# fuzz/seeds/NAME.
fuzz_targets() {
    local f
    for f in fuzz/fuzz_targets/*.rs; do
        f=${f##*/}
        echo "${f%.rs}"
    done
}

# The target the fuzz targets build for. cargo-fuzz picks the one it was
# itself built for, and its release binary for Linux is a musl build,
# which the address sanitizer does not support.
fuzz_target_triple() {
    rustup run "$nightly" rustc -vV | sed -n 's/^host: //p'
}

# Every fuzz target builds, and none fails on its seeds: a target that no
# longer compiles, or a fixed crash that is back, fails here.
job_fuzz_build() {
    local triple t
    triple=$(fuzz_target_triple)
    # cargo fuzz has no --locked and would quietly update a stale
    # fuzz/Cargo.lock; this fails on one instead.
    run_quiet cargo "+$nightly" metadata --locked --format-version 1 --manifest-path fuzz/Cargo.toml
    run cargo "+$nightly" fuzz build --target "$triple"
    for t in $(fuzz_targets); do
        run cargo "+$nightly" fuzz run --target "$triple" "$t" "fuzz/seeds/$t" -- -runs=0
    done
}

# Each fuzz target for FUZZ_SECONDS. The corpus libFuzzer grows is kept in
# fuzz/corpus/NAME (the nightly workflow caches it); the seeds are read,
# never written. A failing input lands in fuzz/artifacts/NAME. Every target
# runs, also after one has failed.
job_fuzz() {
    local triple t secs=${FUZZ_SECONDS:-60} failed=""
    triple=$(fuzz_target_triple)
    for t in $(fuzz_targets); do
        mkdir -p "fuzz/corpus/$t"
        run cargo "+$nightly" fuzz run --target "$triple" "$t" "fuzz/corpus/$t" "fuzz/seeds/$t" -- \
            "-max_total_time=$secs" -timeout=10 -rss_limit_mb=2048 || failed="$failed $t"
    done
    if [ -n "$failed" ]; then
        echo "ci.sh: fuzz targets failed:$failed; the inputs are in fuzz/artifacts/" >&2
        exit 1
    fi
}

# What Miri runs: for each line, `cargo miri test -p` with that line's
# arguments. Miri interprets the tests and stops at undefined behaviour,
# such as a read out of bounds or of uninitialised memory, that a normal
# test run can miss. It is about a hundred times slower than a normal run,
# and it stops the whole test binary at the first system call it cannot
# make (opening a file, for one). So this is the code that needs neither:
#   irsync-glob             the wildcard matcher
#   irsync-proto            the wire messages and the chunker
#   irsync --test sub_chunk the delta codec (irsync::delta) on its own
#   fileset --lib -- ...    fileset's comparison, escaping and wildcards
#   fileset --lib wtf8::    its name conversion for Windows, which is plain
#                           code and is tested on every platform
#   irsync --lib wtf8::     irsync's own copy of that conversion
# A test here that is too slow under Miri is marked
# #[cfg_attr(miri, ignore = "reason")]: those that chunk megabytes, and
# the delta tests on hundreds of KiB. With them, this takes about three
# minutes after the first build.
# Left out: the rest of irsync and fileset, stress, filegen and netstress.
# Their tests open files, sockets, processes and threads through the
# operating system, which Miri cannot do. The unsafe blocks in them are
# such system calls, which Miri cannot check either.
miri_packages=(
    "irsync-glob"
    "irsync-proto"
    "irsync --test sub_chunk"
    "fileset --lib -- compare:: escape:: glob::"
    "fileset --lib wtf8::"
    "irsync --lib wtf8::"
)

# The tests of each entry in miri_packages under Miri, with the nightly in
# fuzz/rust-toolchain.toml. Every entry runs, also after one has failed.
job_miri() {
    local p failed=""
    for p in "${miri_packages[@]}"; do
        # shellcheck disable=SC2086 # an entry is a package and its arguments
        run cargo "+$nightly" miri test --locked -p $p || failed="$failed '$p'"
    done
    if [ -n "$failed" ]; then
        echo "ci.sh: Miri failed for:$failed" >&2
        exit 1
    fi
}

# Clippy for each platform in tools/typecheck-targets, with the crates its
# line names, through tools/typecheck-target.sh, which needs no C compiler
# for the platform. Every platform runs, also after one has failed. It runs
# nothing, so CI still has to run the tests there.
job_typecheck() {
    local target crates p args failed=""
    while read -r target crates; do
        args=()
        for p in $crates; do
            args+=(-p "$p")
        done
        # </dev/null: cargo must not read the list this loop reads.
        run tools/typecheck-target.sh "$target" "${args[@]}" --all-targets --all-features </dev/null ||
            failed="$failed $target"
    done < <(typecheck_lines)
    if [ -n "$failed" ]; then
        echo "ci.sh: typecheck failed for:$failed" >&2
        exit 1
    fi
}

# The release build itself (dist and packaging/dist/build.sh), so a broken
# release build shows up before a tag is pushed.
job_release_build() {
    local target=${CI_RELEASE_TARGET:-$(uname -m)-unknown-linux-musl} contents bin page
    local t standins=() global=() tmp version found pages warnings
    # release.yml must be what release-yml.sh makes from dist-workspace.toml.
    run .github/scripts/release-yml.sh check
    run dist build --artifacts=local "--target=$target"
    printf '\n== the tarball holds both binaries and the man pages\n'
    contents=$(tar -tJf "target/distrib/irsync-$target.tar.xz")
    printf '%s\n' "$contents"
    for bin in irsync fileset; do
        if ! printf '%s\n' "$contents" | grep -x "irsync-$target/$bin" >/dev/null; then
            echo "ci.sh: irsync-$target/$bin is not in the tarball" >&2
            exit 1
        fi
    done
    for page in man/man1/irsync.1 man/man1/fileset.1 man/man5/irsync.toml.5; do
        if ! printf '%s\n' "$contents" | grep -x "irsync-$target/$page" >/dev/null; then
            echo "ci.sh: irsync-$target/$page is not in the tarball" >&2
            exit 1
        fi
    done

    # The pages as this runner's pandoc built them: its version differs
    # from runner to runner, and tools/ci.sh check does not run on all of
    # them for a pull request. The same checks as tools/tests.
    printf '\n== the pages in the tarball are clean for mandoc and groff\n'
    pages=$(mktemp -d)
    tar -xJf "target/distrib/irsync-$target.tar.xz" -C "$pages"
    for page in man/man1/irsync.1 man/man1/fileset.1 man/man5/irsync.toml.5; do
        page=$pages/irsync-$target/$page
        if ! mandoc -T lint -W warning "$page"; then
            echo "ci.sh: mandoc warns about $page" >&2
            rm -rf "$pages"
            exit 1
        fi
        warnings=$(groff -k -t -man -Tutf8 -ww -z "$page" 2>&1 | grep -v 'cannot adjust line' || true)
        if [ -n "$warnings" ]; then
            printf '%s\n' "$warnings" | head -20 >&2
            echo "ci.sh: groff warns about $page" >&2
            rm -rf "$pages"
            exit 1
        fi
    done
    rm -rf "$pages"

    printf '\n== our installer installs from the tarball\n'
    tmp=$(mktemp -d)
    # The release has all three checksums; here only $target was built, so
    # stand-ins fill in the others. dist's global build (the release's
    # build-global-artifacts job) writes the installer, the formula, the
    # source tarball and sha256.sum; with the stand-ins they are not the
    # release's, so they go too.
    for t in aarch64-apple-darwin aarch64-unknown-linux-musl x86_64-unknown-linux-musl; do
        if [ ! -f "target/distrib/irsync-$t.tar.xz.sha256" ]; then
            printf '%064d  irsync-%s.tar.xz\n' 0 "$t" >"target/distrib/irsync-$t.tar.xz.sha256"
            standins+=("target/distrib/irsync-$t.tar.xz.sha256")
        fi
    done
    global=(irsync-installer.sh target/distrib/irsync-installer.sh target/distrib/irsync.rb
        target/distrib/source.tar.gz target/distrib/source.tar.gz.sha256 target/distrib/sha256.sum)
    rm -f "${global[@]}"
    clean_up() { rm -rf "$tmp" ${standins[@]+"${standins[@]}"} "${global[@]}"; }
    packaging/installer/build-installer.sh target/distrib "$tmp/irsync-installer.sh" ||
        { clean_up; exit 1; }
    # dist runs the same generator, as an extra artifact. The copy it puts
    # in target/distrib must be the one we built.
    run dist build --artifacts=global || { clean_up; exit 1; }
    if ! cmp -s target/distrib/irsync-installer.sh "$tmp/irsync-installer.sh"; then
        echo "ci.sh: dist's global build did not produce our irsync-installer.sh" >&2
        clean_up
        exit 1
    fi
    rm -f ${standins[@]+"${standins[@]}"} "${global[@]}"
    mkdir "$tmp/home"
    HOME="$tmp/home" IRSYNC_INSTALLER_ARCHIVE_DIR="$PWD/target/distrib" sh "$tmp/irsync-installer.sh" ||
        { rm -rf "$tmp"; exit 1; }
    version=$(awk -F'"' '/^version = "/ { print $2; exit }' packaging/dist/dist.toml)
    if [ -z "$version" ] ||
        ! "$tmp/home/.local/bin/irsync" --version | grep -F "$version" >/dev/null; then
        echo "ci.sh: the installed irsync does not report version '$version'" >&2
        rm -rf "$tmp"
        exit 1
    fi
    # man finds the pages through the bin directory on PATH (the sibling
    # ../share/man), as it does for a user. Each page must be under the temp
    # home, so a system page cannot satisfy the check.
    for page in irsync fileset irsync.toml; do
        found=$(env -u MANPATH HOME="$tmp/home" PATH="$tmp/home/.local/bin:$PATH" man -w "$page" 2>&1) || found=
        case $found in
            "$tmp/home/"*) ;;
            *)
                echo "ci.sh: man -w $page does not find the installed page: '$found'" >&2
                rm -rf "$tmp"
                exit 1
                ;;
        esac
    done
    rm -rf "$tmp"
}

# The FreeBSD tarball. dist cannot build on FreeBSD yet, so this runs the
# release build script itself and packs its two binaries the way dist would.
# GitHub runs it in the hand-started FreeBSD workflow (freebsd.yml).
job_freebsd_build() {
    local target=x86_64-unknown-freebsd contents file
    run env CARGO_DIST_TARGET=x86_64-unknown-freebsd packaging/dist/build.sh
    run packaging/tarball.sh x86_64-unknown-freebsd packaging/dist target/distrib
    printf '\n== the tarball holds both binaries and the man pages\n'
    contents=$(tar -tJf "target/distrib/irsync-$target.tar.xz")
    printf '%s\n' "$contents"
    for file in irsync fileset man/man1/irsync.1 man/man1/fileset.1 man/man5/irsync.toml.5; do
        if ! printf '%s\n' "$contents" | grep -x "irsync-$target/$file" >/dev/null; then
            echo "ci.sh: irsync-$target/$file is not in the tarball" >&2
            exit 1
        fi
    done
}

# Find every missing tool before the first job starts, not minutes in.
for job in $jobs; do
    "needs_${job//-/_}"
done
[ "$missing" = 0 ] || exit 1

for job in $jobs; do
    printf '\n==== %s\n' "$job"
    "job_${job//-/_}"
done
printf '\nci.sh: passed:%s\n' "$jobs"
