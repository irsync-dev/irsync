# Working on irsync

The contributor guide is the book under `book/src/contributing/`. Start with
`setup.md` and `testing.md`. This file lists the working rules and commands. Releases:
`book/src/contributing/releasing.md`.

## Product and design

Before you change the landing page or the book's look, read
[`docs/PRODUCT.md`](docs/PRODUCT.md) (who irsync is for, what it claims,
and what must not be made up) and [`docs/DESIGN.md`](docs/DESIGN.md) (the
colours, type, components and rules of the site and the book). The tokens
themselves live in `site/tokens.json`; `DESIGN.md` describes them.

## Issues

#24 ("File transfers still break when the network does") is the
project's foundational issue: the goal the rest of the work serves. It
stays open and has no status block on purpose. Do not suggest it, or flag
it, when looking for the next thing to work on.

## Pull requests

All work goes through a pull request. Work on a branch, push the branch,
and open a PR against `main`. Never push to `main` directly unless you are
explicitly asked to.

If the work is for an issue, the PR must close it: put `Closes #N` in the
PR description, where `N` is the issue number.

When asked to merge a PR, clean up afterwards: delete the branch on the
remote and locally, and switch back to an up-to-date `main`.

If CI fails on a PR whose branch is behind `main`, first check whether
`main` already fixes the problem, before you try to fix it on the branch.
If it does, pull `main` and bring the branch up to date, then push. That
starts a fresh CI run that includes the fixes from `main`.

- If nobody has reviewed the PR yet, rebase the branch onto `main` and
  push with `git push --force-with-lease`.
- If the PR already has review comments or approvals, merge `main` into
  the branch instead and push normally. A rebase and force-push would mark
  inline comments as outdated and break the reviewer's "changes since
  your last review" view.

## Release notes

When you prepare a release (`.github/scripts/bump-version.sh`, see
`book/src/contributing/releasing.md`), write its notes too, on the bump
branch, before you open the pull request:

1. List what was merged since the last release: `gh pr list --state merged
   --base main --search "merged:>=DATE"`, with the date of the last
   release, or `git log --merges vLAST..HEAD`. For a stable release, start
   at the last stable release, so the betas in between are covered.
2. Read each pull request's description, not only its title. Look for what
   someone who runs irsync will notice: new flags and config keys, changed
   defaults, anything that now refuses to start or fails where it worked
   before, security fixes, and changes to the protocol or to stored state.
3. Add a section to `CHANGELOG.md`, above the older ones, under the
   heading `## vX.Y.Z` exactly as the version is written. Start with one or
   two sentences on what the release is about. Then, each only when it has
   something in it: `### Upgrading` (what to do or know before upgrading,
   first because it matters most), `### New`, `### Fixed`, and one short
   `### Behind the scenes` for tests, tooling and dependencies. Name flags
   and config keys as the user types them. When a change belongs to an
   issue, name the issue in square brackets at the end of its line, like
   `[#123]`. Do not list pull requests one by one: GitHub adds that list
   below the section.
4. Write it in the book's style: short sentences, plain words, for people
   who use irsync and have not read the code. For a security fix, say what
   is safer now, and leave out how the problem could have been used.
5. Commit it on the bump branch (`git commit -m 'Release notes for vX.Y.Z'
   -- CHANGELOG.md`) and say in the pull request that the section wants a
   read. The maintainer reviews it there.

A test and the release gate both refuse a version without its section
(`.github/scripts/changelog-section.sh`).

## Setup

`tools/dev-setup.sh` installs the Rust tooling (rustup, toolchain, the
nightly in `fuzz/rust-toolchain.toml`, cargo-nextest, cargo-deny,
cargo-fuzz, zizmor, dist, mdbook), actionlint and shellcheck, at CI's
versions, and checks the system tools; `tools/dev-setup.sh --check` only
reports. It does not run on Windows; see "Windows" in
`book/src/contributing/setup.md`.

## Tests

A change that touches only Markdown files, `book/` or `site/` needs no
tests, clippy or fmt; CI skips them too. The exception is what the man
pages are built from: the book chapters named in `tools/build-man.sh` and
`packaging/man/`. Run `python3 -m unittest discover -s tools/tests` for those;
they need pandoc and mandoc. `tools/build-man.sh OUTDIR` builds the three
pages into `OUTDIR` to look at. The site workflow
(`.github/workflows/site.yml`) still builds and checks the book and site
after the push to `main`.

Everyday run, while working on a change:

```sh
cargo test -q --workspace --all-features --no-fail-fast
```

This skips five slow network-fault (netstress) tests, marked `#[ignore]`.
They take a few minutes, and the first run builds the fault binaries into
`target/netsim/debug`.

Full regression, before you push:

```sh
tools/ci.sh check
```

This is the script CI runs: formatting, the tests of `tools/dev-setup.sh`
and of the man page build (it builds the pages and checks them), clippy
with and without OpenTelemetry, every test including those five slow ones
(no other test is ignored), and the doc tests. It needs cargo-nextest,
pandoc and mandoc.

`tools/ci.sh` with no job runs CI's other jobs too: `deny` (dependencies),
`deny-license-check` (licenses for every platform; blocks a release only),
`workflows` (after a change under `.github/`; actionlint and zizmor),
`shell-scripts` (shellcheck on every shell script), `fuzz-build` (builds
the fuzz targets in `fuzz/` and runs their seeds; Linux only) and
`release-build` (after a change to the release build or packaging; Linux
only).

On Windows only `filegen`, `fileset` and `irsync-glob` build. The everyday run
is:

```sh
cargo test -q -p filegen -p fileset -p irsync-glob --no-fail-fast
```

and the full run is `tools/ci.sh check` in Git Bash, which covers those three
crates and ends with a smoke run of `filegen` and `fileset` together. It needs
cargo-nextest, not Python, pandoc, mandoc or rsync. A bare `tools/ci.sh` runs
`check` only. Tests that need a symlink skip, with a line that starts
`skipping:`, when the user may not create symlinks, and fail when `CI` is set;
Developer Mode lets them run. Keep the checkout path and `CARGO_TARGET_DIR`
short (CI uses `C:\t`), because the MSVC linker fails on a path over 260
characters.

`FUZZ_SECONDS=300 tools/ci.sh fuzz` fuzzes each target for that long; it
runs only when named. See "Fuzzing" in `book/src/contributing/testing.md`.

`tools/ci.sh miri` runs the tests in `miri_packages` (in `tools/ci.sh`)
under Miri, which catches undefined behaviour in unsafe code. It is slow
and runs only when named; the nightly workflow runs it too. See "Static
analysis" in `book/src/contributing/testing.md`.

Every `unsafe` block needs a `// SAFETY:` comment saying why that call is
sound; clippy refuses one without.

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

- **CI runs on GitHub** (`.github/workflows/ci.yml`, Linux, macOS and
  Windows) for every push to `main` and every pull request. Its jobs run
  `tools/ci.sh`, so change a check there, not in the workflow. Run it locally
  first anyway: a macOS-only failure is cheaper to find here. The `windows`
  job (`windows-latest` and `windows-11-arm`) reports and does not block: it
  is not among the jobs `CI result` needs, and a release run leaves it out.
- **GNU rsync 3.1 or newer must be on `PATH`.** Without it,
  `crates/irsync/tests/rsync_compare.rs` passes without checking anything.
  macOS ships an older rsync: `brew install rsync`.
- **`fuzz/` has a `Cargo.lock` of its own.** After any change to
  `Cargo.lock`, run `tools/sync-fuzz-lock.sh` and commit both files.
  `tools/ci.sh check` fails if they differ.
- **`cargo test -p stress` alone does not build the binaries it needs.** Use
  `cargo test --workspace`, or run `cargo build --workspace` first.
- **The site holds the book once per release channel**, and `/book/` is
  the released one, not `main`. Read "The books" in
  `book/src/contributing/website.md` before you link to a new book page
  from the landing page.
- **Formatting is checked on push** by `.githooks/pre-push`. Install it once
  per clone: `git config core.hooksPath .githooks`.
