# dev-setup downloads the cargo tools instead of compiling them

Issue: https://github.com/irsync-dev/irsync/issues/35

## Intent

`tools/dev-setup.sh` builds cargo-nextest, cargo-deny, cargo-dist and
mdbook from source with `cargo install`. On a fresh machine that is several
minutes of compiling, and again whenever a pinned version changes. All four
projects publish ready-made binaries. After this change the script
downloads those, checks each against a SHA-256 it holds, and compiles only
where there is no binary for the platform.

## Goals

- On Linux and macOS, x86_64 and arm64, none of the four tools is compiled.
- A download that does not match its SHA-256 is refused and nothing is
  installed, as for actionlint today.
- mdbook is held to one version, like the other three. (Until now any
  mdbook was accepted locally.)
- Where there is no binary for the platform, `cargo install --locked` at
  the pinned version still works, as today.

## Non-goals

- CI's installs of nextest, deny and dist stay as they are. They already
  use prebuilt binaries.
- No version checks come back into `site/build.sh` or `site/serve.sh`.
  Only `dev-setup.sh` holds mdbook to the pin. (My call.)

## Anti-goals

- A wrong hash must never turn quietly into a compile. It is reported as
  refused and the tool is left missing.
- No new tool to install or trust.

## Approaches

1. **Direct downloads, hashes in the script.** Chosen. It is what the
   script already does for actionlint, adds nothing to trust, and refuses a
   tampered download. Cost: 16 hash lines to replace by hand when versions
   move.
2. **cargo-binstall.** Much less script, and it falls back to compiling by
   itself. It can verify minisign signatures, but only for a tool whose
   own `Cargo.toml` names a signing key
   (`[package.metadata.binstall.signing]`). None of the four does at the
   pinned versions, so for these tools it would check nothing beyond
   HTTPS. Rejected for that, and because it is one more tool to install
   and trust. Worth another look if the tools start signing.
3. **Keep compiling, with a shared build cache.** Rejected: still compiles
   on a fresh machine, which is the complaint.

## Design

**What is downloaded.** From each project's GitHub release, at the pinned
version. Linux uses the static musl builds, so the host's libc does not
matter.

| Tool | Linux x86_64, arm64 | macOS x86_64, arm64 | FreeBSD x86_64 |
|---|---|---|---|
| cargo-nextest 0.9.151 | musl `.tar.gz` | one universal `.tar.gz` | `.tar.gz` |
| cargo-deny 0.20.2 | musl `.tar.gz` | `.tar.gz` | compile |
| cargo-dist 0.34.0 | musl `.tar.xz` | `.tar.xz` | compile |
| mdbook 0.5.2 | musl `.tar.gz` | `.tar.gz` | compile |

Any other platform compiles.

**Where the versions live.** Unchanged: nextest and deny are read from
`ci.yml`, dist from `dist-workspace.toml`, mdbook from `MDBOOK_VERSION` in
`site.yml`.

**Where the hashes live.** In `dev-setup.sh`, one table per tool next to
the version the hashes are for, as for actionlint. A comment says where the
hashes come from: nextest, deny and dist publish `.sha256` files on the
release. mdbook publishes none, so its hashes are computed from the
downloads when the version is set.

**When the pin and the table disagree.** A test in
`tools/tests/test_dev_setup.py` fails, as the actionlint test does today,
so a version bump that forgets the hashes does not pass CI. At run time the
script does not download with stale hashes: it says so and compiles.

**Installing.** One function replaces `install_actionlint` and serves all
five tools: download to a temporary directory, check the hash, unpack only
the one binary into `$CARGO_HOME/bin`. `cargo_tool` tries it first and
runs `cargo install` only when there is no binary for the platform. The
order of outcomes for a tool at the wrong version or missing:

1. binary for this platform, hash matches: installed, `ok`.
2. hash does not match: `refused`, tool reported missing, no compile.
3. download fails (offline): reported missing with the command to rerun.
   No compile. (My call: a silent five-minute build is a worse surprise
   than a clear message.)
4. no binary for this platform: `cargo install --locked` as today.

`--check` still changes nothing.

**mdbook.** The `any` mode of `cargo_tool` goes away. Another mdbook
version is replaced, and `--check` warns about it, as for the other tools.

**CI.** `site.yml` checks the mdbook download against the same Linux
x86_64 hash before unpacking. (My call: CI should not be looser than a
developer's machine.) A test holds that hash equal to the one in
`dev-setup.sh`.

**Docs.**
- `docs/SUPPLY_CHAIN_DECISIONS.md` gets decision 17: developer tools are
  downloaded as release binaries and checked against SHA-256s kept in the
  repo; why not cargo-binstall; cost if wrong: the hashes are replaced by
  hand on each bump, and mdbook's are our own first download.
- `book/src/contributing/setup.md`: drop "Any mdbook will do"; say the
  tools are downloaded, not compiled.
- The header comment of `dev-setup.sh`.

## Left to the builder

- The shape of the hash tables and of the shared install function.
- The layout inside each archive (dist and deny have a top directory,
  nextest and mdbook do not).
- `cargo install` refuses to overwrite a binary it did not install itself.
  Where the fallback or a printed hint could meet a downloaded binary, add
  `--force` or word the hint accordingly.
- Tests, in the style of the actionlint ones: a tampered download is
  refused for each tool, a platform without a binary compiles, a good
  download installs without calling cargo, and mdbook at another version
  is replaced.
