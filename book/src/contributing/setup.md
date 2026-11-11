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
([reference](../reference/fileset.md)), `filegen`
([Generating test trees](filegen.md)), `irsync-stress`
([Stress testing](stress.md)) and `irsync-netstress`
([Network stress](netstress.md)).

## Requirements

- **Rust.** `rust-toolchain.toml` pins the stable channel; rustup picks it
  up.
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
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
cargo test --workspace --all-features --no-fail-fast -- --include-ignored
```

`--include-ignored` adds the slow network-fault tests that an everyday
`cargo test` skips ([Testing](testing.md)). The second clippy run keeps the
build without the `otel` feature compiling (see
[Cargo features](testing.md#cargo-features)).

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
