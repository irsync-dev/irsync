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
#   release-build  the release build for this machine's Linux musl target
#
# With no job it runs all of them, the quick ones first. release-build runs
# on Linux only; a run of everything leaves it out elsewhere.
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
        check | deny | workflows | release-build) jobs="$jobs $arg" ;;
        *) echo "ci.sh: unknown job '$arg' (jobs: $all_jobs)" >&2; exit 2 ;;
    esac
done
if [ -z "$jobs" ]; then
    jobs=$all_jobs
    if [ "$os" != Linux ]; then
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
