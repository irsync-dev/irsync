# Release automation design

Date: 2026-11-11

## Intent

Today the only way to get irsync is `cargo install --git ...`, which needs a
Rust toolchain. The README and book say "Release binaries will be provided once
the beta starts." This project makes releases real: pushing a version tag
builds irsync for the supported platforms, checks it with the full test suite,
and publishes it through the channels people actually install from.

The maintainer does two things by hand: bump the version and push a tag.
Everything after that is automatic.

## Goals

- A tag `vX.Y.Z` on a commit produces a GitHub Release for that version, with
  no further manual steps.
- Nothing is published unless the full CI regression passes on the tagged
  commit (fmt, both clippy runs, all tests including the six netsim tests, on
  Linux and macOS: what `.github/workflows/ci.yml` runs today).
- Supported release platforms:
  - Linux x86_64
  - Linux aarch64 (arm64)
  - macOS aarch64 (Apple Silicon)
- Five ways to install, all fed by the same release:
  1. **Tarballs** attached to the GitHub Release, one per platform, with
     checksums.
  2. **One-line shell installer** (`curl ... | sh`) at
     `https://irsync.org/install.sh`: a small version-independent script that
     picks a release (latest stable by default, or a newer beta/alpha on
     request) and runs that release's cargo dist installer. Each release's own
     cargo dist installer is also attached to its GitHub Release.
  3. **Homebrew** from the tap repo `irsync-dev/homebrew-tap`
     (`brew install irsync-dev/tap/irsync`), macOS and Linux.
  4. **apt** repository at `https://dist.irsync.org/apt` (amd64, arm64).
  5. **dnf** repository at `https://dist.irsync.org/rpm` (x86_64, aarch64).
- apt and dnf users add the repository once; after that `apt upgrade` /
  `dnf upgrade` pick up new releases.
- The release setup is generated and maintained with **cargo dist**
  (`dist init` / `dist generate`), so it can be upgraded with the tool rather
  than hand-edited.

## Non-goals

- Publishing crates to crates.io. All workspace crates stay unpublished.
- Shipping the test tools (`irsync-stress`, `irsync-netstress`, `filegen`).
  Each release contains exactly two binaries, `irsync` and `fileset`, and
  every channel ships both together (see "Binaries in a release").
- macOS x86_64, Windows, or other Linux architectures. (The README currently
  says macOS x86_64 is supported; see "Docs" below.)
- macOS code signing and notarisation. curl and Homebrew downloads are not
  quarantined, so they are not needed for the channels above.
- Automatic version bumping, changelog writing, or tag creation.
- Changing what CI does for pushes to `main` and pull requests.

## Anti-goals (would count as failure even if it "works")

- A release, tap update, or package-repo update happening for a commit whose
  tests did not all pass, or for an untagged push.
- A second, hand-copied definition of "the CI tests" that can drift from
  `ci.yml`.
- Releases coupled to the website deploy: a release must not need a site
  deploy or promotion, and the site must never serve a stale installer.
- Package files committed into the main `irsync` repo's history.

## Constraints and existing setup

- Repo: `github.com/irsync-dev/irsync`. Workspace version lives in the root
  `Cargo.toml` (`[workspace.package] version = "0.1.0"`); all crates inherit it.
  No tags exist yet.
- `ci.yml` runs on `ubuntu-latest` and `macos-latest`, installs GNU rsync
  (needed by `rsync_compare.rs`), then fmt, clippy (all features and no
  default features) and `cargo test --workspace --all-features --no-fail-fast
  -- --include-ignored`.
- `crates/irsync/build.rs` bakes in the git commit; release builds must run
  from a real checkout of the tagged commit.
- The `irsync` crate's default features are `["otel"]`. Release builds use the
  default features. The `netsim` and `test-hooks` features must never be in a
  release build.
- The website (irsync.org) is served by a Cloudflare Worker with static assets,
  built from `site/` and deployed by `.github/workflows/site.yml`: staging on
  push to `main`, production by manual promotion. GitHub Pages is not used by
  the project today.
- With ssh, the irsync client copies its own binary to the server when both
  run the same OS and CPU (`crates/irsync/src/ssh/`). The released Linux binary
  therefore has to run on whatever Linux the server runs, not only on the
  client's.

## Decided by the maintainer

- cargo dist plus GitHub Actions; distribution through GitHub.
- Only tags trigger a release, and only when all CI tests pass.
- Platforms: Linux amd64, Linux arm64, macOS arm64.
- Channels: tarball, `curl | sh` installer, Homebrew, apt repo, dnf repo.
- apt and dnf as proper repositories, hosted on GitHub, at
  `dist.irsync.org`.
- The installer URL is `https://irsync.org/install.sh`. It must never serve
  a stale installer and releases must not need a site deploy.
- Version bump and tagging by hand; everything else automatic.
- Linux binaries are static musl builds.
- The release test gate also runs the full suite on the shipped Linux
  targets (musl x86_64, Linux arm64), switched on by an input to `ci.yml`
  that only the release workflow sets.
- Pre-releases (alpha, beta) go to the GitHub Release only, not to Homebrew,
  apt or dnf. The install script can opt into the latest beta or alpha when
  it is newer than the latest stable release.
- Package maintainer and signing-key identity: `irsync releases <releases@irsync.org>`.

## Design

### Release flow

```
maintainer: bump [workspace.package] version, commit, push to main
maintainer: git tag vX.Y.Z && git push origin vX.Y.Z
                    |
       release workflow (on push of tag v*)
                    |
   1. test gate: the full ci.yml checks on the tagged commit
                    |  (all must pass; otherwise stop, nothing published)
   2. cargo dist: build irsync + fileset for the 3 targets, tarballs, checksums,
      shell installer, Homebrew formula
                    |
   3. build .deb and .rpm for the 2 Linux targets from the same binaries
                    |
   4. publish GitHub Release (tarballs, installer, checksums, .deb, .rpm,
      auto-generated release notes)
                    |
   5. stable tags only: push formula to irsync-dev/homebrew-tap;
      add packages to irsync-dev/packages, regenerate + sign apt/dnf
      metadata, push (GitHub Pages serves dist.irsync.org)
```

### Test gate: one definition of "the tests"

`ci.yml` gains `workflow_call` as a trigger so the release workflow can call
it as a reusable workflow. cargo dist's generated `release.yml` runs it before
building, using cargo dist's supported hook for jobs that run before the build
(`plan-jobs` or the equivalent in the cargo dist version used). The build jobs
depend on it. The existing `push` / `pull_request` triggers stay as they are.

The gate reruns the full suite on the tag even if the same commit already
passed on `main`. That costs a few minutes and avoids relying on an earlier
run's result.

`ci.yml` gets a `workflow_call` input (e.g. `release-targets: true`) that adds
matrix entries for the shipped Linux targets: `x86_64-unknown-linux-musl` on
`ubuntu-latest` and Linux aarch64 on `ubuntu-24.04-arm` (musl there too, so
the tested binary matches the shipped one). Those entries run the same steps:
rsync install, fmt, both clippy runs, full test suite with
`--include-ignored`. On ordinary pushes and pull requests the input is unset
and the matrix is exactly today's. The test definition stays in one file.

### Builds (cargo dist)

- Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`,
  `aarch64-apple-darwin`.
- **Linux libc (maintainer's decision): static musl builds.** A glibc binary
  built on GitHub's current Ubuntu runners needs a recent glibc and fails on
  older servers (e.g. RHEL 9 and its clones ship glibc 2.34). Since the client
  copies itself to the server over ssh, the Linux binary must run on any
  reasonable Linux. One static binary also serves the tarball, installer,
  Homebrew-on-Linux, .deb and .rpm alike.
- musl's default memory allocator can be slow under heavy multithreaded
  allocation. Measuring that and, if needed, switching to another allocator
  (e.g. mimalloc) on musl is a **follow-up, not part of this project**.
- Linux aarch64 builds on GitHub's native arm runners (`ubuntu-24.04-arm`,
  free for public repos) rather than by cross-compiling, unless cargo dist's
  defaults make cross-compiling the simpler route.
- Installers: `shell` and `homebrew`.
- Homebrew tap: `irsync-dev/homebrew-tap`.
- Only one package is distributable (see "Binaries in a release"); the other
  workspace crates are marked `dist = false` (or the cargo dist equivalent).
- cargo dist checks that the tag matches the workspace version; a mismatch
  fails the release.

### Binaries in a release (maintainer's decision)

Every irsync package contains both `irsync` and `fileset`: the tarballs, the
installer, the Homebrew formula, the `.deb` and the `.rpm`. There is no
separate fileset package.

cargo dist puts the binaries of one Cargo package into one archive, and
`irsync` and `fileset` are separate crates. They must stay independent:
fileset "shares no code with irsync" (README) so it can verify irsync's copies
without inheriting its bugs. So the `irsync` crate must not depend on
`fileset`, nor the other way round.

Designer's recommendation: a small release-only crate (e.g.
`crates/irsync-release`, the one package cargo dist distributes, named so the
release artifacts are still called `irsync`) with two `[[bin]]` targets,
`irsync` and `fileset`, each a thin `main` calling the existing entry point
in the `irsync` and `fileset` library crates. The release binaries must behave
exactly like the ones built from the crates themselves; in particular the git
commit `crates/irsync/build.rs` bakes in must still show in `irsync
--version`. The builder may pick another mechanism that gets both binaries
into every artifact, as long as irsync and fileset stay independent and there
is one release package.

Only `irsync` is copied to servers over ssh; that behaviour doesn't change.

### Debian and RPM packages

Built in the release workflow, after the cargo dist build, from the same
`irsync` and `fileset` binaries cargo dist produced (not a separate compile). Tool:
`cargo-deb` and `cargo-generate-rpm`, or `nfpm` (builder's choice; nfpm can
package a prebuilt binary for both formats from one config, which fits
"reuse the built binary" well). Package metadata (name `irsync`, description,
licence MIT, homepage https://irsync.org, maintainer
`irsync releases <releases@irsync.org>`) is kept in the repo.

The packages install `/usr/bin/irsync` and `/usr/bin/fileset` and nothing
else for now: no systemd
unit, no man page, no config files.

The `.deb` and `.rpm` files are also attached to the GitHub Release.

### Package repository: irsync-dev/packages on dist.irsync.org

- New public repo `irsync-dev/packages`, served by GitHub Pages with custom
  domain `dist.irsync.org` (a `CNAME` file in the repo; DNS record `dist`
  CNAME `irsync-dev.github.io`, "DNS only" if DNS is on Cloudflare, so GitHub
  can issue the HTTPS certificate).
- Layout:
  - `/apt/`: a Debian repository (`dists/stable/main/binary-{amd64,arm64}`,
    `pool/`), metadata generated with `apt-ftparchive` (or `reprepro`),
    `InRelease` and `Release.gpg` signed.
  - `/rpm/`: per-arch directories with `repodata/` from `createrepo_c`,
    `repomd.xml` signed (`repomd.xml.asc`), packages signed with `rpmsign`.
  - `/irsync.gpg`: the public signing key (ASCII-armoured, plus a binary
    keyring for apt's `signed-by`).
  - `/irsync.repo`: a ready-made dnf repo file.
  - `/index.html`: a short page with the setup commands for apt and dnf.
- On each release the workflow checks out the repo, adds the new packages,
  prunes to the last few versions per architecture (keeps the site well under
  GitHub Pages' ~1 GB limit), regenerates and signs the metadata, and pushes.
- The update is serialised (workflow `concurrency`), so two releases close
  together cannot clobber each other.

### Pre-releases

- Tags: `vX.Y.Z` (stable), `vX.Y.Z-beta.N`, `vX.Y.Z-alpha.N`. Any tag with a
  `-` suffix is a pre-release.
- Pre-releases go through the same test gate and produce a GitHub Release
  marked as a pre-release, with tarballs, checksums and the cargo dist
  installer.
- They are **not** published to the Homebrew tap, apt or dnf. (cargo dist
  skips the tap for pre-releases by default; the package-repo job must skip
  them too. `.deb`/`.rpm` files need not be built for pre-releases.)

### Website: `/install.sh`

`site/install.sh` is a small POSIX `sh` script, kept in the repo and deployed
with the site. It names no version, so it never goes stale and releases need
no site deploy. (This replaces the redirect discussed earlier: a redirect
cannot choose between stable, beta and alpha.)

What it does:

1. Ask the GitHub releases API for `irsync-dev/irsync`'s releases.
2. Pick one:
   - default: the latest stable release;
   - `--beta`: the newest beta, if it is newer than the latest stable;
     otherwise the latest stable;
   - `--alpha` (designer's call): the newest release that is alpha, beta or
     stable, so `--alpha` never installs something older than `--beta` would.
   - "Newer" compares versions (semver order), not publish dates.
3. Download that release's cargo dist installer from the release and run it,
   passing through any other arguments.
4. Say which version it picked, and fail clearly if GitHub can't be reached
   (including the unauthenticated API rate limit).

Usage:

```
curl --proto '=https' --tlsv1.2 -LsSf https://irsync.org/install.sh | sh
curl --proto '=https' --tlsv1.2 -LsSf https://irsync.org/install.sh | sh -s -- --beta
```

The script needs only `sh`, `curl` (or `wget`) and standard tools; no `jq`.
It gets a small test (e.g. version selection against a canned API response)
run by the site workflow. The site check (`site/tools/check.py`) may need to
know about it. It should be served with a text content type (`_headers`).

### Secrets and one-time setup

Done once by the maintainer, documented in the book:

- Create `irsync-dev/homebrew-tap` and `irsync-dev/packages` (public), turn on
  Pages for `packages` with custom domain `dist.irsync.org` and HTTPS enforced.
- DNS record for `dist.irsync.org`.
- A GPG signing key for the package repos, user ID
  `irsync releases <releases@irsync.org>`. Secrets in `irsync-dev/irsync`: the private
  key and its passphrase. The book page records how to rotate it.
- A token that can push to `homebrew-tap` and `packages` only (a fine-grained
  personal access token or a GitHub App), stored as a secret. cargo dist
  expects `HOMEBREW_TAP_TOKEN` for the tap; the packages repo may use the
  same or a separate secret.
- Tag protection for `v*` is recommended but not required.

### Docs

- README and `book/src/introduction.md`: replace "Release binaries will be
  provided once the beta starts" with the install options (installer,
  Homebrew, apt, dnf, tarball), keeping `cargo install --git` for other
  platforms. The fileset install instructions (README, fileset part of the
  book) point at the same packages. Fix the platform line so it doesn't promise macOS x86_64 release
  binaries.
- New contributor page `book/src/contributing/releasing.md`: how to cut a
  release (bump, commit, tag, push), what the workflow does, the one-time
  setup above, and what to do if a release fails halfway.
- `AGENTS.md`: a pointer to the release page.

## Left to the builder

- The exact cargo dist version and config layout (`dist-workspace.toml` vs
  `[workspace.metadata.dist]`).
- How the test gate hooks into cargo dist's generated workflow, as long as it
  reuses `ci.yml` and nothing builds or publishes before it passes.
- nfpm vs cargo-deb/cargo-generate-rpm; apt-ftparchive vs reprepro.
- How many old versions to keep in the package repos.
- Exact flag names for the install script, as long as stable is the default
  and beta/alpha are opt-in.
- Layout and wording of `dist.irsync.org/index.html`.
