# A FreeBSD test run and build, started by hand

Date: 2026-11-16

## Intent

irsync will have to run on a few FreeBSD storage boxes, and should later be
in FreeBSD's ports collection. Nobody has built or tested it on FreeBSD
yet. Before FreeBSD becomes part of every release, there should be a way
to check one version on FreeBSD and get a binary to try on a real box.

That check is for betas and stable releases, and it is run by hand. It
must not slow down or block everyday work.

## Goals

- One command starts a run that tests a chosen version of irsync in a
  FreeBSD VM and says pass or fail.
- A passing run hands back `irsync-x86_64-unknown-freebsd.tar.xz`, laid
  out like the release tarballs (`irsync`, `fileset`, `README.md`,
  `LICENSE` in a folder named after the target), with a SHA-256 file.
- irsync builds and its tests pass on FreeBSD.

## Non-goals

- The tarball on the GitHub Release page, and `curl | sh` on FreeBSD.
  That is step B, later: FreeBSD as a fourth release target.
- The ssh auto-upload ("immediate mode") to a FreeBSD server. The table
  in `crates/irsync/src/ssh/release.rs` stays as it is. The boxes are set
  up by hand.
- The port itself, a `pkg add` package, and a build with the ports tree's
  own Rust.
- arm64 FreeBSD.
- Signed build provenance (attestation) for the tarball. It comes with
  step B.

## Anti-goals

- The run starts on a push or a pull request, or becomes part of the
  `CI result` check. A red FreeBSD run must never block a merge or a
  release.
- FreeBSD-only commands that live in the workflow file, so they can't be
  run on a FreeBSD machine by hand.

## Constraints

- A release can't be changed once published, so a build made afterwards
  can't be attached to it. This is why step A delivers a download from the
  run and not a release file.
- A manual run uses the workflow file of the ref it is started on. Tags
  older than this change don't have it. So the workflow runs from `main`
  and takes the version to test as an input.
- The scripts at the tested version do the work. Versions older than this
  change can't be built this way.
- `test_workflow_policy.py`: every Action is pinned to a commit with a
  `# v…` comment.
- The VM action runs its commands as root. The tests expect an ordinary
  user (the daemon refuses to start as root), so the run needs one.
- Nothing is cached in the VM. A run builds everything from scratch.

## Approaches

**A. Manual run, tarball as a download from the run.** Chosen. It can't
block anything, it gets binaries onto the boxes, and it shows whether
FreeBSD works before releases depend on it.

**B. FreeBSD as a fourth release target.** The tarball is on the release
page and the install script works. But it builds on every release, and a
FreeBSD failure stops the release. Planned as the next step, once a few
betas have passed with A.

**C. Like A, plus a native package for `pkg add`.** Dropped: the port
will do this job.

## Design

Decided with Mike: approach A; x86_64 only; ssh auto-upload left out.
The rest are Claude's calls.

**Workflow** `.github/workflows/freebsd.yml`

- Only trigger: `workflow_dispatch`, with two inputs. `ref` is the tag or
  branch to test (default: the branch the run is started on). `freebsd`
  is the FreeBSD release (default `14`, the newest 14.x).
- One job on `ubuntu-latest`, `contents: read`, with a time limit of two
  hours. It checks out `ref` without keeping credentials, then starts
  `vmactions/freebsd-vm` (v1.6.0, pinned), x86_64.
- In the VM, as root: install the packages and create an ordinary user.
  As that user: install Rust with rustup, then run the two `tools/ci.sh`
  jobs below.
- Last step: upload the tarball and its `.sha256` with
  `actions/upload-artifact`. It only runs when everything before passed.

Why 14 by default: a binary built on the oldest supported FreeBSD also
runs on newer ones, and not the other way round.

Why rustup and not the `rust` package: it gives the Rust version in
`rust-toolchain.toml`, the one every other platform is tested with.

**`tools/ci.sh`**

- `check` runs on FreeBSD as it does elsewhere. If it needs a slimmer
  test set there, that is a nextest profile like `ci-macos`, chosen by
  `uname`, not a change in the workflow.
- A new job, `freebsd-build`, FreeBSD only. It runs
  `packaging/dist/build.sh` with `CARGO_DIST_TARGET=x86_64-unknown-freebsd`
  (the same build and version check as a release), packs the tarball into
  `target/distrib/`, writes the SHA-256 file, and checks that both
  binaries are in the tarball. `ci.sh` with no job leaves it out
  elsewhere, as it does `release-build`.

**Policy tests** (`test_workflow_policy.py`)

- `freebsd.yml` has `workflow_dispatch` as its only trigger.
- No other workflow calls it.

**Docs**

- `book/src/contributing/releasing.md`: how to run it for a version and
  fetch the tarball:

  ```sh
  gh workflow run freebsd.yml -f ref=v0.1.0-beta.4
  gh run download <run-id>
  ```

- `AGENTS.md`: one line on the new job.

## Getting it green

How much of irsync fails on FreeBSD is unknown. Several places have a
Linux path, a macOS path and a fallback for everything else (file
cloning, CPU count, file metadata, the stress tools' `/proc` reads);
FreeBSD takes the fallbacks, which have never run. The netstress fault
binaries are the likeliest trouble.

So the work has two parts:

1. The workflow, the `ci.sh` job, the policy tests and the docs. The run
   starts and reports honestly, even if red.
2. Fixes until the run is green. A small fix goes in. Anything that
   changes how irsync behaves on FreeBSD, or that means leaving a test
   out there, goes back to Mike first.

The workflow can only be started once it is on `main`, so part 1 merges
first and part 2 follows in its own pull requests.

## Left to the builder

- The exact package list, and how to get cargo-nextest (a package, or
  `cargo install --locked` at CI's version).
- How the VM step switches to the ordinary user and passes the result
  back.
- Whether to turn on the VM action's `cache-after-prepare`.
- The names of the policy tests.
