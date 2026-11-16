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

`tools/dev-setup.sh` sets most of this up and checks the rest. It installs
what rustup and cargo can install: rustup, the toolchain with rustfmt and
clippy, the musl target on Linux, and cargo-nextest, cargo-deny, dist,
actionlint and mdbook. The CI tools are held to the versions CI uses: if you
have another version, it is replaced. Any mdbook will do. It points git at the repo's hooks. System tools such as
rsync, Python and a C compiler it only checks, naming the package to
install. `tools/dev-setup.sh --check` reports without changing anything. Run
it again whenever you like.

It works on Linux, macOS and FreeBSD, and names the package for each one's
package manager (`apt`, `brew`, `pkg`).

- **Rust.** `rust-toolchain.toml` pins an exact Rust version, which rustup
  picks up; CI and releases use the same file. To move to a newer Rust, change
  the version there and run the full regression.
- **GNU rsync 3.1 or newer on `PATH`**, for `crates/irsync/tests/rsync_compare.rs`,
  which compares irsync's result with rsync's. Without it the test prints a
  line and passes, which `cargo test -q` hides, so it checks nothing. With
  the `CI` environment variable set, the test fails instead of skipping. On
  macOS the bundled rsync (2.6.9 or openrsync) is too old:
  `brew install rsync`.
- **`curl`, and a `tar` that writes and reads `.tar.xz`**, for
  `crates/irsync/tests/ssh_release.rs`, which builds a small release
  tarball and downloads it from a local directory. GNU tar needs the `xz`
  program for that (`apt install xz-utils`); macOS's tar has xz built in.

## Continuous integration

The workflow in `.github/workflows/ci.yml` runs on GitHub for every push to
`main` and every pull request, on Linux and macOS. The workflow installs
the tools; the checks themselves are in `tools/ci.sh`, one job there per job
in the workflow, so the same script runs them on your machine:

```sh
tools/ci.sh                  # every job
tools/ci.sh check            # one job
tools/ci.sh --help
```

To change what CI checks, change `tools/ci.sh`.

The `check` job installs GNU rsync and runs, in this order:

```sh
cargo fmt --all --check
python3 -m unittest discover -s tools/tests    # tests tools/dev-setup.sh
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo nextest run --locked --workspace --all-features --profile ci --run-ignored all
cargo test --locked --workspace --all-features --doc
```

`--locked` makes the run fail if `Cargo.lock` is out of date, as the release
build would.

The `release-build` job runs the release build itself for the two
Linux targets (x86_64 and arm64 musl): `dist build`, which runs
`packaging/dist/build.sh`, on the same runners as a release. A change that
would break a release fails here, before anyone tags it. On your machine it
builds the one musl target that matches it, on Linux only, and needs dist
and `musl-gcc`.

The `freebsd-build` job builds the FreeBSD tarball with the same build
script. It runs on FreeBSD x86_64 only, and CI does not run it. GitHub runs
it in a workflow you start by hand: see
[Testing a version on FreeBSD](releasing.md#testing-a-version-on-freebsd).

The `deny` job checks the dependencies with `cargo deny`, and the
`workflows` job runs the workflow rule tests and actionlint, which needs
PyYAML and actionlint ([Supply chain](supply-chain.md)).

Two things run only on GitHub: the macOS and arm64 runs, and the dependency
review of a pull request.

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

For everyday work you don't need nextest: `cargo test` runs the same
tests. `tools/ci.sh check` needs it; `tools/dev-setup.sh` installs it.

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
tools/ci.sh check
```

This is the full regression, the same script CI's `check` job runs. After
a change under `.github/`, to the dependencies or to the release build, run
`tools/ci.sh` with no job, which runs CI's other jobs too.
