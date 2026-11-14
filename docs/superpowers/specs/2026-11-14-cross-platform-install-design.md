# Cross-platform install over ssh: design

2026-11-14. Follows the "Later item" in
`2026-11-10-ssh-onboarding-design.md`.

## Intent

When a run goes through ssh and the server needs irsync, the client copies
its own binary over. That only works when both machines have the same OS
and CPU; otherwise irsync stops and tells the user to set the server up by
hand. This change lets a released client fetch the release binary for the
server's platform and install that instead.

The user does not want `install.sh` run on the server: it installs things
the user never agreed to. The server gets one binary, in irsync's own
place, exactly as the same-platform copy does today.

## Goals

- A release build (alpha, beta or full) installs or updates irsync on a
  server with another OS or CPU, with the client's exact version.
- The server needs no internet access and no new tools.
- A failure changes nothing on the server.

## Non-goals

- Dev builds across platforms. A dev build still installs only on the same
  OS and CPU (user's decision).
- Updating a reachable older daemon without ssh. Still open in `TODO.md`.
- Signature checks. Tracked as a critical item in `TODO.md` (see below).
- Installing `fileset`, a download cache, or downloading on the server.

## Anti-goals

- Running `install.sh` or the release's installer anywhere.
- Installing a binary whose version differs from the client's.

## Approaches

1. **The client's `curl` and `tar`, SHA-256 check.** Chosen by the user. No
   new dependencies, and it matches how irsync already uses the system
   `ssh`. The checksum catches a damaged download; protection against
   tampering is HTTPS to GitHub, the same trust the client's own binary
   came with.
2. **Download and unpack inside irsync.** No reliance on client tools, but
   adds xz and tar libraries to every build for a rarely used path. Same
   trust as 1.
3. **1 plus a signature against a key built into irsync.** Protects against
   a swapped release file. Needs a release key and a release workflow
   step. Deferred: it should cover `install.sh` too.

Decided earlier and confirmed: the client downloads, not the server.

## Design

All of it is in `crates/irsync/src/ssh/client.rs`, in `ensure_installed`.

**When.** The server's version differs from the client's and the server's
OS or CPU differs from the client's. The same-platform path keeps copying
the running binary.

**Which file.** Tag `v<CARGO_PKG_VERSION>`. Platform to release target:

| Server        | Target                       |
|---------------|------------------------------|
| linux x86_64  | `x86_64-unknown-linux-musl`  |
| linux aarch64 | `aarch64-unknown-linux-musl` |
| macos aarch64 | `aarch64-apple-darwin`       |

Anything else (macOS x86_64, unknown OS or CPU) stops with today's "set the
server up by hand" message, naming the platform.

Files, from `https://github.com/irsync-dev/irsync/releases/download/<tag>/`:
`irsync-<target>.tar.xz` and `sha256.sum`.

**Steps on the client.**

1. If the build is dirty or its commit is unknown, stop: a dev build cannot
   install on another platform.
2. Print one line: `downloading irsync <tag> for <os> <cpu>`.
3. Download both files into a fresh temporary directory with `curl`
   (https only, fail on HTTP errors), or `wget` if there is no `curl`.
4. Hash the tarball in-process (SHA-256) and compare with its line in
   `sha256.sum`. A missing line or a mismatch stops.
5. Unpack only `irsync-<target>/irsync` with `tar`.
6. Upload it through the existing copy command.
7. Remove the temporary directory, also on failure.

**Version check on the server (both paths).** The upload command gains one
step between `chmod` and `mv`: run the temp file with `--version` and
require exactly `irsync <LONG_VERSION>`. On a mismatch, or if it does not
run, remove the temp file and exit with a new code. Nothing is replaced.

- Downloaded binary: the message says this client is a dev build that
  shares the release's version number, so it cannot install on another
  platform.
- Copied binary: the message says the copied binary does not run on the
  server. This closes the matching item in `TODO.md`.

**Report.** `Install` gains `Downloaded { replaced }`. The summary line
reads like the copied one, plus "(release binary for <os> <cpu>)".

**Errors**, each one line saying what to do:

- no `curl` or `wget`, or no `tar` that reads `.xz`, on the client;
- the download failed (network, or GitHub unreachable);
- no such release (HTTP 404): likely a dev build with an unreleased
  version number;
- checksum mismatch;
- no release binary for the server's platform.

## Tests

No test touches the network. A test-only override points the download at
local files (designer's call, like `IRSYNC_INSTALL_RELEASES_FILE` in
`site/install.sh`). The fake tarball holds the test's own irsync binary,
and `FAKE_SSH_UNAME` reports a platform other than the host's, so the
version check passes for real.

- downloads, installs, and a second run is `Current`;
- checksum mismatch, missing release, unsupported platform (riscv64), dev
  build: each fails with its message and leaves the server untouched;
- a binary that reports another version is removed and not installed, on
  both paths;
- the existing "other platform" tests in `tests/ssh_client.rs` and
  `tests/onboard_e2e.rs` change to match.

## Docs

- `TODO.md`: remove the cross-platform items and the "copied binary may not
  run" item. Add at the top, marked critical: release files are checked
  only by SHA-256 over HTTPS; sign them and verify against a key built into
  irsync, for this download and for `install.sh`.
- `docs/IRSYNC_DECISIONS.md` R44: amend.
- The book: the first-sync tutorial's "set the server up by hand" section
  and any page that says the platforms must match.

## Designer's calls

- No cache: the download runs only when the server's version differs.
- SHA-256 in-process (`sha2` is already in the dependency tree) rather than
  `sha256sum` or `shasum`, which differ between Linux and macOS.
- The version check also applies to the same-platform copy.
- A same-platform copy that does not run does not fall back to the release
  binary. It could later.

## Left to the builder

- How the test override is passed and gated.
- Exact wording of messages, and the exit codes of the upload command.
- Whether the download code gets its own module under `ssh/`.
