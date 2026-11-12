# Set up a development checkout

irsync is developed at
[github.com/irsync-dev/irsync](https://github.com/irsync-dev/irsync). Clone it
and build the workspace:

```sh
git clone https://github.com/irsync-dev/irsync
cd irsync
cargo build --workspace
```

The workspace holds irsync itself and the tools that test it: `fileset`
([reference](../fileset/reference.md)), `filegen`
([Generating test trees](filegen.md)), `irsync-stress`
([Stress testing](stress.md)) and `irsync-netstress`
([Network stress](netstress.md)).

## Requirements

- **Rust.** `rust-toolchain.toml` pins an exact Rust version, which rustup
  picks up; CI and releases use the same file. To move to a newer Rust, change
  the version there and run the full regression.
- **GNU rsync 3.1 or newer on `PATH`**, for `crates/irsync/tests/rsync_compare.rs`,
  which compares irsync's result with rsync's. Without it the test prints a
  line and passes, which `cargo test -q` hides, so it checks nothing. With
  the `CI` environment variable set, the test fails instead of skipping. On
  macOS the bundled rsync (2.6.9 or openrsync) is too old:
  `brew install rsync`.

## Continuous integration

The workflow in `.github/workflows/ci.yml` runs on GitHub for every push to
`main` and every pull request, on Linux and macOS. It installs GNU rsync and
runs, in this order:

```sh
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo nextest run --locked --workspace --all-features --profile ci --run-ignored all
cargo test --locked --workspace --all-features --doc
```

`--locked` makes the run fail if `Cargo.lock` is out of date, as the release
build would.

A second job, `release build`, runs the release build itself for the two
Linux targets (x86_64 and arm64 musl): `dist build`, which runs
`packaging/dist/build.sh`, on the same runners as a release. A change that
would break a release fails here, before anyone tags it.

The tests run with [cargo-nextest](https://nexte.st), which runs each test
in its own process and tests from all files side by side. That makes the
run faster than `cargo test`, which runs one test file after another. The
`ci` profile in `.config/nextest.toml` runs every test even after a failure
and kills a test that runs longer than 5 minutes, naming it. `--run-ignored
all` adds the slow network-fault tests that an everyday `cargo test` skips
([Testing](testing.md)). nextest does not run doc tests, so `cargo test
--doc` runs those.

On macOS, CI uses the `ci-macos` profile instead. It leaves out a few slow
tests that work the same on every OS, which Linux CI still runs (see
[Platform skips](testing.md#platform-skips)).

The second clippy run keeps the build without the `otel` feature compiling
(see [Cargo features](testing.md#cargo-features)).

You don't need nextest locally: `cargo test` runs the same tests. To run
them the way CI does, install it with `cargo install cargo-nextest --locked`.

## The pre-push hook

`.githooks/pre-push` refuses a push whose commits are not rustfmt-clean, so a
formatting slip is caught before CI sees it. Every commit being pushed is
checked with `cargo fmt --all -- --check` in a throwaway worktree: what counts
is what the remote receives, not the state of the working tree. Install it
once per clone:

```sh
git config core.hooksPath .githooks
```

## Before pushing

Clippy and the tests are not part of the hook. Run them yourself:

```sh
touch crates/irsync/build.rs    # re-runs the build script, which bakes in the git commit
cargo test -q --workspace --all-features --no-fail-fast -- --include-ignored
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
```

These are the checks CI runs.
