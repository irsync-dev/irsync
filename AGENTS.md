# Working on irsync

The contributor guide is the book under `book/src/contributing/`. Start with
`setup.md` and `testing.md`. This file only lists the commands. Releases:
`book/src/contributing/releasing.md`.

## Setup

`tools/dev-setup.sh` installs the Rust tooling (rustup, toolchain,
cargo-nextest, cargo-deny, dist, mdbook) and actionlint, at CI's versions,
and checks the system tools;
`tools/dev-setup.sh --check` only reports.

## Tests

A change that touches only Markdown files, `book/` or `site/` needs no
tests, clippy or fmt; CI skips them too. The site workflow
(`.github/workflows/site.yml`) still builds and checks the book and site
after the push to `main`.

Everyday run, while working on a change:

```sh
cargo test -q --workspace --all-features --no-fail-fast
```

This skips five slow network-fault (netstress) tests, marked `#[ignore]`.
They take a few minutes, and the first run builds the fault binaries into
`target/netsim/debug`.

Full regression, before committing to `main` or pushing:

```sh
tools/ci.sh check
```

This is the script CI runs: formatting, the tests of `tools/dev-setup.sh`,
clippy with and without OpenTelemetry, every test including those five slow
ones (no other test is ignored), and the doc tests. It needs cargo-nextest.

`tools/ci.sh` with no job runs CI's other jobs too: `deny` (dependencies),
`workflows` (after a change under `.github/`) and `release-build` (after a
change to the release build or packaging; Linux only).

`tools/ci.sh freebsd-build` builds the FreeBSD tarball, on FreeBSD x86_64
only. CI does not run it; the hand-started FreeBSD workflow does (see
`book/src/contributing/releasing.md`).

A new test of irsync through the fault layer should run in-process on
tokio's paused clock (`with_netsim` in `crates/irsync/tests/common`, as
in `tests/netsim.rs`), which takes seconds, rather than be ignored. Mark
a test that runs the real fault binaries with
`#[ignore = "slow: netsim; run with --include-ignored"]`.

To run only the netsim tests:

```sh
cargo test -p irsync --features netsim
cargo test -p netstress -- --include-ignored
```

## Things that trip people up

- **CI runs on GitHub** (`.github/workflows/ci.yml`, Linux and macOS) for
  every push to `main` and every pull request. Its jobs run `tools/ci.sh`,
  so change a check there, not in the workflow. Run it locally first
  anyway: a macOS-only failure is cheaper to find here.
- **GNU rsync 3.1 or newer must be on `PATH`.** Without it,
  `crates/irsync/tests/rsync_compare.rs` passes without checking anything.
  macOS ships an older rsync: `brew install rsync`.
- **`cargo test -p stress` alone does not build the binaries it needs.** Use
  `cargo test --workspace`, or run `cargo build --workspace` first.
- **Formatting is checked on push** by `.githooks/pre-push`. Install it once
  per clone: `git config core.hooksPath .githooks`.
