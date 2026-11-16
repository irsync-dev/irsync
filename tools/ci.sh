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
#                  the slow ones, and the doc tests
#   deny           the dependency check (cargo deny, see deny.toml)
#   workflows      the workflow rule tests and actionlint
#   release-build  the release build for this machine's Linux musl target,
#                  dist's global build, and our installer run on the result
#   freebsd-build  the FreeBSD tarball; FreeBSD x86_64 only
#
# With no job it runs all of them, the quick ones first. release-build runs
# on Linux only and freebsd-build on FreeBSD only; a run of everything
# leaves each out elsewhere.
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
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd -P)
cd "$root"
os=$(uname -s)

usage() { awk 'NR > 1 && /^#/ { sub(/^# ?/, ""); print; next } NR > 1 { exit }' "$0"; }

all_jobs="workflows deny check release-build"
jobs=""
for arg in "$@"; do
    case $arg in
        -h | --help) usage; exit 0 ;;
        check | deny | workflows | release-build | freebsd-build) jobs="$jobs $arg" ;;
        *) echo "ci.sh: unknown job '$arg' (jobs: $all_jobs freebsd-build)" >&2; exit 2 ;;
    esac
done
if [ -z "$jobs" ]; then
    jobs=$all_jobs
    if [ "$os" = FreeBSD ]; then
        jobs="workflows deny check freebsd-build"
        echo "ci.sh: leaving out release-build, which runs on Linux only"
    elif [ "$os" != Linux ]; then
        jobs="workflows deny check"
        echo "ci.sh: leaving out release-build, which runs on Linux only"
    fi
fi

# Prints a command, then runs it.
run() {
    printf '\n== %s\n' "$*"
    "$@"
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
    need rsync rsync --version
    need python3 python3 --version
    need pandoc pandoc --version
    # Presence only: it reads an empty file and succeeds.
    need mandoc mandoc -T ascii /dev/null
}
needs_deny() { need cargo-deny cargo deny --version; }
needs_workflows() {
    need "PyYAML (the Python yaml module)" python3 -c "import yaml"
    need actionlint actionlint -version
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

job_check() {
    # ${target[@]+...} below: bash 3.2 on macOS calls an empty array unset.
    local target=() profile=ci
    if [ -n "${CARGO_BUILD_TARGET:-}" ]; then
        target=(--target "$CARGO_BUILD_TARGET")
    fi
    # GitHub's macOS runners are slow, so there the ci-macos profile leaves
    # out slow tests that Linux covers (.config/nextest.toml). A Mac of your
    # own runs them all.
    if [ "${GITHUB_ACTIONS:-}" = true ] && [ "$os" = Darwin ]; then
        profile=ci-macos
    fi
    # Re-runs the build script, which bakes in the git commit; a test
    # compares the two. A fresh checkout does not need it, a used one does.
    touch crates/irsync/build.rs

    run cargo fmt --all --check
    # Tests tools/dev-setup.sh and tools/build-man.sh. They are Python, so no
    # cargo command runs them. A missing pandoc or mandoc fails here instead
    # of skipping the man page tests.
    MAN_TESTS_REQUIRED=1 run python3 -m unittest discover -s tools/tests
    run cargo clippy --locked --workspace --all-targets --all-features ${target[@]+"${target[@]}"} -- -D warnings
    # Keeps the build without the otel feature compiling.
    run cargo clippy --locked --workspace --all-targets --no-default-features ${target[@]+"${target[@]}"} -- -D warnings
    # nextest runs each test in its own process, tests from all files side
    # by side, and kills a hung test (see .config/nextest.toml).
    # --run-ignored all adds the slow netstress tests.
    run cargo nextest run --locked --workspace --all-features --profile "$profile" \
        --run-ignored all ${target[@]+"${target[@]}"}
    # nextest does not run doc tests.
    run cargo test --locked --workspace --all-features --doc ${target[@]+"${target[@]}"}
}

# Known vulnerabilities, unmaintained or yanked crates, licenses and sources,
# as set in deny.toml.
job_deny() {
    run cargo deny --locked check
}

# The workflow rules, including the supply chain policy in
# .github/scripts/tests/test_workflow_policy.py, then actionlint.
job_workflows() {
    run python3 -m unittest discover -s .github/scripts/tests
    # shellcheck is off: it would also lint the generated release.yml.
    run actionlint -shellcheck= -pyflakes=
}

# The release build itself (dist and packaging/dist/build.sh), so a broken
# release build shows up before a tag is pushed.
job_release_build() {
    local target=${CI_RELEASE_TARGET:-$(uname -m)-unknown-linux-musl} contents bin page
    local t standins=() global=() tmp version found
    # release.yml must be what dist generates from dist-workspace.toml.
    run dist generate --check
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
