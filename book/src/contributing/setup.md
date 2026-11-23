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
rustup, the toolchain with rustfmt and clippy, the musl target on Linux, the
nightly the fuzz targets build with, and cargo-nextest, cargo-deny,
cargo-fuzz, zizmor, dist, actionlint, shellcheck and mdbook. The nightly
is a large download, and only fuzzing and Miri need it. Those eight tools
are downloaded from their release pages and checked against SHA-256s kept
in the script. Cargo builds a tool only where its project has no binary for
your machine, or when the script's hashes are for another version than
CI's. There is no shellcheck binary for FreeBSD; there the script names the
package. They are held to the versions CI uses: if you have another
version, it is replaced. It points git at the repo's hooks. System tools
such as rsync, Python and a C compiler it only checks, naming the package
to install. `tools/dev-setup.sh --check` reports without changing anything.
Run it again whenever you like.

It works on Linux, macOS and FreeBSD, and names the package for each one's
package manager (`apt`, `brew`, `pkg`). It does not run on Windows; see
[Windows](#windows).

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
  program for that (`sudo apt install xz-utils`); macOS's tar has xz built in.
- **pandoc and mandoc**, for the man pages: `tools/build-man.sh OUTDIR`
  builds them from the book with pandoc, and the tests in `tools/tests`
  check them with mandoc. `tools/dev-setup.sh` checks both and says how to
  install them. The `check` job fails without them.
- **groff**, optional. `test_groff_renders_without_warnings` renders each
  page with it and skips without it. It runs on Linux CI. On a Mac,
  `nix-shell -p groff` is one way to get it.
- **minisign 0.10 or newer**, optional. Maintainers sign releases with it
  ([The release signing key](release-signing-key.md)), and it makes the
  signature test files. Older versions write a signature format that irsync
  refuses. The `sign-release.sh` tests also run with the real minisign
  when it is there; with the `CI` environment variable set, they fail
  instead of skipping without it.

## Windows

`filegen`, `fileset` and `irsync-glob` build and test on Windows, on x86_64
and ARM64. `irsync` itself, `irsync-proto`, `irsync-stress` and
`irsync-netstress` do not yet, so `cargo build --workspace` does not work
there. Name the three crates:

```sh
cargo test -q -p filegen -p fileset -p irsync-glob --no-fail-fast
```

What you need:

- **The MSVC Rust toolchain and the MSVC build tools.** Only the
  `x86_64-pc-windows-msvc` and `aarch64-pc-windows-msvc` targets are used, not
  the GNU toolchain and not 32-bit Windows. Install rustup, and Visual Studio
  Build Tools with the C++ tools and a Windows SDK. rustup picks up the
  version in `rust-toolchain.toml`.
- **Git Bash,** which comes with Git for Windows, to run `tools/ci.sh`. The
  repository's `.gitattributes` keeps the shell scripts with LF line
  endings in the working tree, which bash needs.
- **cargo-nextest,** which `tools/ci.sh check` runs. Install it by hand
  (`cargo install --locked cargo-nextest`, or a release download).
  `tools/dev-setup.sh` does not run on Windows. `tools/ci.sh check` there needs
  no Python, pandoc, mandoc or rsync.
- **Developer Mode,** for the symlink tests. Making a symlink needs Developer
  Mode, an elevated process or the symlink privilege. Without it, a test that
  needs a symlink prints a line that starts `skipping:` and passes. With the
  `CI` environment variable set, it fails instead, as the rsync comparison
  test does ([Requirements](#requirements)). `filegen` finds the same thing at
  the start of a run and leaves the symlink operations out (see
  [On Windows](filegen.md#on-windows)).
- **A short checkout path and a short `CARGO_TARGET_DIR`.** The MSVC linker
  fails on a path over 260 characters. CI sets `CARGO_TARGET_DIR` to
  `C:\t`. Clone somewhere like `C:\w\irsync`.
- **Trees on an NTFS drive.** The tests make their trees in the temporary
  directory, so that drive has to be NTFS too. ReFS, FAT, exFAT, network
  shares and the folders a virtual machine shares with its host are not
  supported.

The full regression, in Git Bash:

```sh
tools/ci.sh check
```

On Windows it runs `cargo fmt --all --check`, the fuzz lock check, one clippy
run, nextest and the doc tests for the three crates, and then a smoke run:
`filegen` fills and changes a tree, and `fileset` records it and checks it. A
bare `tools/ci.sh` runs `check` only and says what it leaves out.

## Continuous integration

The workflow in `.github/workflows/ci.yml` runs on GitHub for every push to
`main` and every pull request, on Linux and macOS, and on Windows for the
three crates. The workflow installs
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
python3 -m unittest discover -s tools/tests    # tests tools/dev-setup.sh and the man pages
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo clippy --locked --workspace --all-targets --no-default-features -- -D warnings
cargo nextest run --locked --workspace --all-features --profile ci --run-ignored all
cargo test --locked --workspace --all-features --doc
```

`--locked` makes the run fail if `Cargo.lock` is out of date, as the release
build would.

The `windows` job runs `tools/ci.sh check` on `windows-latest` (x86_64) and
`windows-11-arm` (ARM64), in Git Bash, with `CARGO_TARGET_DIR` set to `C:\t`.
It covers `filegen`, `fileset` and `irsync-glob`, as above. It reports on
pull requests and pushes, but `CI result` does not need it, so a failure there
does not block a merge. A release run leaves it out. It joins the required
jobs when `irsync` itself builds on Windows.

The `release-build` job runs the release build itself for the two
Linux targets (x86_64 and arm64 musl): `dist build`, which runs
`packaging/dist/build.sh`, on the same runners as a release. It checks that `release.yml` is what
`.github/scripts/release-yml.sh write` makes, that each tarball holds the three man pages, and
that our installer, run on the tarball with a temporary home, installs a
working `irsync` and pages that `man` finds. A change that
would break a release fails here, before anyone tags it. On your machine it
builds the one musl target that matches it, on Linux only, and needs dist
and `musl-gcc`.

The `freebsd-build` job builds the FreeBSD tarball with the same build
script. It runs on FreeBSD x86_64 only, and CI does not run it. GitHub runs
it in a workflow you start by hand: see
[Testing a version on FreeBSD](releasing.md#testing-a-version-on-freebsd).

The `fuzz-build` job builds the fuzz targets in `fuzz/` with their pinned
nightly and runs each on its checked-in inputs. The long fuzz runs are in a
nightly workflow; see [Fuzzing](testing.md#fuzzing).

The `miri` job runs the tests that can run under Miri, which catches
undefined behaviour in unsafe code. It is slow, so it runs only when you
name it, and every night on GitHub; see
[Static analysis](testing.md#static-analysis).

The `deny` job checks the dependencies with `cargo deny`, for the platforms
irsync is built for and for both Windows targets, so it also checks the crates
only a Windows build uses. The `deny-license-check` job checks the licenses of
every platform's dependencies; on GitHub its failure blocks a release, not
a pull request. The `workflows` job runs the workflow rule tests, actionlint
and zizmor, which needs PyYAML, actionlint and zizmor
([Supply chain](supply-chain.md),
[Static analysis](testing.md#static-analysis)). The `shell-scripts` job runs shellcheck on every shell script, on every pull
request. A warning that is wrong for one line is turned off on that line,
with the reason in a comment.

Two things run only on GitHub: the macOS, Windows and arm64 runs, and the
dependency review of a pull request.

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
