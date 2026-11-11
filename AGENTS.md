# Working on irsync

The contributor guide is the book under `book/src/contributing/`. Start with
`setup.md` and `testing.md`. This file only lists the commands.

## Tests

Everyday run, while working on a change:

```sh
cargo test -q --workspace --all-features --no-fail-fast
```

This skips six slow network-fault (netsim) tests, marked `#[ignore]`. They
take several minutes, and the first run builds the fault binaries into
`target/netsim/debug`.

Full regression, before committing to `main` or pushing:

```sh
touch crates/irsync/build.rs    # re-runs the build script, which bakes in the git commit
cargo test -q --workspace --all-features --no-fail-fast -- --include-ignored
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
```

`--include-ignored` adds exactly the six netsim tests; no other test is
ignored. Mark any new test that runs a sync through the fault layer with
`#[ignore = "slow: netsim; run with --include-ignored"]`.

To run only the netsim tests:

```sh
cargo test -p irsync --features netsim -- --include-ignored
cargo test -p netstress -- --include-ignored
```

## Things that trip people up

- **CI does not run yet.** The repository is not hosted on GitHub, so
  `.github/workflows/ci.yml` never fires. Run the full regression above
  yourself.
- **GNU rsync 3.1 or newer must be on `PATH`.** Without it,
  `crates/irsync/tests/rsync_compare.rs` passes without checking anything.
  macOS ships an older rsync: `brew install rsync`.
- **`cargo test -p stress` alone does not build the binaries it needs.** Use
  `cargo test --workspace`, or run `cargo build --workspace` first.
- **Formatting is checked on push** by `.githooks/pre-push`. Install it once
  per clone: `git config core.hooksPath .githooks`.
