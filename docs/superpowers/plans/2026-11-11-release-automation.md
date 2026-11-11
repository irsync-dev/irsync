# Release Automation Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** A pushed `vX.Y.Z` tag runs the full test suite, builds `irsync` and `fileset` for three platforms, and publishes them as a GitHub Release, through `irsync.org/install.sh`, Homebrew, and signed apt/dnf repositories at `dist.irsync.org`.

**Architecture:** cargo dist generates `.github/workflows/release.yml`. The release is one *generic* dist package named `irsync` (`packaging/dist/dist.toml`). Its build script runs `cargo build -p irsync -p fileset`, so both programs ship together and neither crate depends on the other. All Cargo crates are marked `dist = false`. The existing `ci.yml` becomes a reusable workflow that the release calls as its test gate; when called this way it adds the shipped Linux musl targets. A custom publish job builds `.deb`/`.rpm` with nfpm and updates the `irsync-dev/packages` Pages repo. `site/install.sh` is a version-independent POSIX sh script that picks a release from the GitHub API.

**Tech Stack:** cargo dist (latest release at implementation time, pinned in config), GitHub Actions, nfpm, apt-ftparchive, createrepo_c, rpmsign, gpg, POSIX sh, Python `unittest` (as in `site/tools`).

**Spec:** `docs/superpowers/specs/2026-11-11-release-automation-design.md`

## Global Constraints

- Targets: `x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`, `aarch64-apple-darwin`.
- Linux binaries are statically linked (musl). Linux aarch64 builds and tests on `ubuntu-24.04-arm`.
- Each release ships exactly `irsync` and `fileset`, never `irsync-stress`, `irsync-netstress` or `filegen`. The `irsync` crate must not depend on `fileset`, nor the other way round.
- Release builds use default features. `netsim` and `test-hooks` must never be in a release build.
- Tags: `vX.Y.Z` stable; `vX.Y.Z-beta.N`, `vX.Y.Z-alpha.N` pre-release. Pre-releases go to the GitHub Release only: no Homebrew, apt, dnf, and no `.deb`/`.rpm`.
- Nothing is uploaded or published (GitHub Release, tap, packages repo) unless the test gate passed on the tagged commit.
- CI on pushes to `main` and pull requests runs exactly what it runs today.
- Homebrew tap `irsync-dev/homebrew-tap`, formula `irsync`. Package repo `irsync-dev/packages`, served at `https://dist.irsync.org`; apt under `/apt`, dnf under `/rpm`.
- Package maintainer and GPG user ID: `irsync releases <releases@irsync.org>`. Licence MIT, homepage `https://irsync.org`.
- Packages install `/usr/bin/irsync` and `/usr/bin/fileset` only.
- Installer URL: `https://irsync.org/install.sh`; stable by default, `--beta` / `--alpha` opt in.
- Release notes: GitHub's automatically generated notes.
- Commit messages: short summary line, blank line, short plain-English why. No attribution trailers of any kind.

## Review Focus

- **Package pruning by version, not by name or date.** Keeping the newest N must keep `0.10.0` over `0.9.0`. Test in Task 5.
- **Re-running a failed release.** Running the repo update twice for the same version must leave exactly one copy of each package and valid signatures. Test in Task 5.
- **No stable release exists yet.** Before `v0.1.0`, plain `install.sh` must fail with a message that names `--alpha`/`--beta`, not install nothing silently. Test in Task 4.
- **An accidentally dynamic Linux binary.** If a musl build ends up dynamically linked, the build must fail rather than ship. Check in Task 1's build script.
- **Version drift.** `packaging/dist/dist.toml`'s version must equal `[workspace.package] version`, or binaries report one version while the release is named another. Test in Task 1.

---

### Task 1: One generic dist package that ships both binaries

This task is also the spike for the riskiest assumption: that cargo dist can ship a generic package next to a Cargo workspace, with shell and Homebrew installers and musl targets. **If `dist plan` / `dist build` can't produce what Step 6 expects, stop and report to the maintainer before going further.** Don't fall back to a mechanism that makes `irsync` depend on `fileset`, or gives two packages a same-named binary target. Cargo warns about the output filename collision, and it races in `target/`.

**Files:**
- Create: `dist-workspace.toml` (written by `dist init`, then edited)
- Create: `packaging/dist/dist.toml`
- Create: `packaging/dist/build.sh`
- Modify: `Cargo.toml` (add `[profile.dist]`; `[workspace.metadata.dist] dist = false` or per-crate `[package.metadata.dist] dist = false`, whichever the dist version uses)
- Create: `crates/irsync/tests/release_version.rs`

**Interfaces:**
- Produces: dist app `irsync`, version from `packaging/dist/dist.toml`. Artifacts named `irsync-<target>.tar.xz`, installer `irsync-installer.sh`, Homebrew formula `irsync`. Task 4 hard-codes the installer file name and Task 6 the archive names, so confirm them from `dist plan` output and correct those tasks if they differ.
- Produces: `packaging/dist/build.sh`, run by dist with `CARGO_DIST_TARGET` set.

- [ ] **Step 1: Write the failing version test** `crates/irsync/tests/release_version.rs`

```rust
#[test]
fn release_package_version_matches_the_workspace() {
    let dist: toml::Table = std::fs::read_to_string(
        concat!(env!("CARGO_MANIFEST_DIR"), "/../../packaging/dist/dist.toml"))
        .unwrap().parse().unwrap();
    assert_eq!(dist["package"]["version"].as_str(), Some(env!("CARGO_PKG_VERSION")),
        "bump packaging/dist/dist.toml together with [workspace.package] version");
}
```

- [ ] **Step 2: Run it, expect FAIL** (file not found)

Run: `cargo test -q -p irsync --test release_version`

- [ ] **Step 3: Install cargo dist and initialise**

Install the latest dist release (`cargo install cargo-dist --locked`, or its installer). Run `dist init`, choosing GitHub CI, the three targets, and installers `shell` and `homebrew`. Then edit the config so that:
- members are the generic package `packaging/dist` only (`dist:packaging/dist`), and every Cargo crate is `dist = false`;
- `cargo-dist-version` is pinned to the installed version;
- `targets` = the three in Global Constraints;
- `installers = ["shell", "homebrew"]`, `tap = "irsync-dev/homebrew-tap"`, `formula = "irsync"`, `publish-jobs = ["homebrew"]` (Task 6 adds `./packages`), `publish-prereleases = false`;
- `install-path` puts both binaries in `~/.local/bin` (check how the chosen syntax resolves, using Step 7);
- `github-custom-runners`: `aarch64-unknown-linux-musl = "ubuntu-24.04-arm"`, `x86_64-unknown-linux-musl = "ubuntu-latest"`, `aarch64-apple-darwin = "macos-latest"`;
- the musl C toolchain is installed on the Linux runners (dist's `[dependencies.apt] musl-tools`, or in `build.sh`).

`packaging/dist/dist.toml`: `[package] name = "irsync"`, `version = "0.1.0"`, `license = "MIT"`, `repository = "https://github.com/irsync-dev/irsync"`, `homepage = "https://irsync.org"`, `description` from the README's first line, `binaries = ["irsync", "fileset"]`, `build-command = ["./build.sh"]`.

`Cargo.toml`: `[profile.dist] inherits = "release"`, `lto = "thin"`.

- [ ] **Step 4: Write `packaging/dist/build.sh`** (POSIX sh, `set -eu`)

1. `rustup target add "$CARGO_DIST_TARGET"`.
2. `cargo build --locked --profile dist --target "$CARGO_DIST_TARGET" -p irsync -p fileset`, run from the repo root (a real checkout, so `build.rs` bakes in the commit).
3. Copy `target/$CARGO_DIST_TARGET/dist/{irsync,fileset}` to where dist looks for generic binaries. Check where that is with Step 6.
4. Refuse to ship a broken binary. On `*-linux-musl`, fail unless `file` reports both binaries as statically linked. On every target, run `./irsync --version` and fail unless the output contains the `dist.toml` version and does not contain `unknown`. All three runners are native for their target, so this always runs.

- [ ] **Step 5: Run the version test, expect PASS**

Run: `cargo test -q -p irsync --test release_version`

- [ ] **Step 6: Check the plan**

Run: `dist plan`
Expected: one app `irsync` 0.1.0 with binaries `irsync` and `fileset`, exactly the three targets, a shell installer and a Homebrew formula. No other app listed.

- [ ] **Step 7: Build locally and inspect**

Run: `dist build --artifacts=all --target x86_64-unknown-linux-musl` (needs `musl-tools`).
Expected: `target/distrib/irsync-x86_64-unknown-linux-musl.tar.xz` containing exactly `irsync` and `fileset` (plus licence/readme files), both static. `irsync-installer.sh` runs with `HOME` set to a temp dir and `--no-modify-path`, and leaves both binaries in `$HOME/.local/bin`.

- [ ] **Step 8: Commit** (`release.yml` is committed in Task 3; leave it out here)

```bash
git add dist-workspace.toml packaging/dist Cargo.toml Cargo.lock crates/irsync/tests/release_version.rs
git commit -m "Add the cargo dist release package for irsync and fileset" -m "One generic dist package builds both binaries, so the two crates stay independent."
```

---

### Task 2: Make ci.yml callable as the release test gate

**Files:**
- Modify: `.github/workflows/ci.yml`

**Interfaces:**
- Produces: `ci.yml` accepts `workflow_call` with input `plan` (string, not required; dist passes its plan JSON to custom jobs). When called this way, its matrix adds the two shipped Linux targets.

- [ ] **Step 1: Add the trigger and a target-aware matrix**

- `on:` gains `workflow_call: { inputs: { plan: { type: string, required: false } } }`. `push` and `pull_request` stay unchanged.
- The matrix becomes a list of `{os, target}` objects. Choose it with `fromJSON(...)` on whether `inputs.plan` is set (inside a called workflow `github.event_name` is the caller's event, so it can't tell the two cases apart):
  - not set (today's runs): `{os: ubuntu-latest, target: ""}`, `{os: macos-latest, target: ""}`;
  - set (release): those two plus `{os: ubuntu-latest, target: x86_64-unknown-linux-musl}` and `{os: ubuntu-24.04-arm, target: aarch64-unknown-linux-musl}`.
- The job name stays `${{ matrix.os }}` when `target` is empty, so the check names that branch protection may rely on don't change. Otherwise it's `${{ matrix.os }} ${{ matrix.target }}`.
- When `target` is set: install `musl-tools`, add the target to the toolchain, and pass `--target ${{ matrix.target }}` to both clippy runs and the test run. All other steps are the same.

- [ ] **Step 2: Check the netsim tests really run on the musl target**

`AGENTS.md` says the netsim fault binaries build into `target/netsim/debug`. Read how they are built. If they ignore `--target`, the musl entries would quietly test glibc fault binaries. Make the build honour `CARGO_BUILD_TARGET`, or set it in the musl matrix entries so the nested build picks it up.

- [ ] **Step 3: Check it locally on x86_64 musl**

Run: `cargo test -q --workspace --all-features --no-fail-fast --target x86_64-unknown-linux-musl -- --include-ignored`
Expected: same pass count as the glibc run, 0 failed. Then `cargo clippy --workspace --all-targets --all-features --target x86_64-unknown-linux-musl -- -D warnings` must be clean.

- [ ] **Step 4: Check today's CI is unchanged**

Push the branch and open a draft PR. Expected: exactly the two jobs `ubuntu-latest` and `macos-latest` run, both green.

- [ ] **Step 5: Commit**

```bash
git commit -am "Let the release workflow call CI as its test gate" -m "A release also tests the musl and arm64 Linux targets it ships; ordinary pushes run the same jobs as before."
```

---

### Task 3: Generate release.yml with the gate in front

**Files:**
- Modify: `dist-workspace.toml` (`plan-jobs = ["./ci"]`)
- Create: `.github/workflows/release.yml` (generated)

**Interfaces:**
- Consumes: Task 2's `ci.yml` `workflow_call`.
- Produces: `release.yml` triggered by tags matching `v[0-9]*`, in which no `host`, publish or `announce` job can run unless the `ci` job succeeded.

- [ ] **Step 1: Configure the gate and regenerate**

Set `plan-jobs = ["./ci"]` and run `dist generate`. Set GitHub release notes to be generated automatically, using dist's option if it has one. Otherwise add `--generate-notes` behaviour through dist's supported hook, not by hand-editing.

- [ ] **Step 2: Check the gate in the generated graph**

Read the `needs:` chain in `release.yml`. Required: every job that uploads or publishes (`host`, `publish-*`, `announce`) depends on the `ci` job, directly or through another job. Builds may run alongside the tests.

If dist doesn't wire it so, try `host-jobs = ["./ci"]` and check again. If neither works, set `allow-dirty = ["ci"]`, add `ci` to `host`'s `needs:` by hand, and say in Task 7's `releasing.md` that `release.yml` is hand-patched and must be re-patched after `dist generate`. Prefer the first two.

- [ ] **Step 3: Check generation is clean**

Run: `dist generate --check` (or the dist version's equivalent)
Expected: no diff.

- [ ] **Step 4: Commit**

```bash
git add dist-workspace.toml .github/workflows/release.yml
git commit -m "Add the tag-triggered release workflow" -m "Generated by cargo dist; nothing is published unless the full CI gate passes on the tagged commit."
```

---

### Task 4: install.sh on irsync.org

**Files:**
- Create: `site/install.sh`
- Modify: `site/build.sh` (it copies `site/*` but skips `*.sh`; change the skip to name `build.sh`, `serve.sh`, `deploy.sh` explicitly)
- Modify: `site/_headers` (`/install.sh` → `Content-Type: text/plain; charset=utf-8`)
- Create: `site/tools/test_install.py`
- Modify: `site/tools/test_headers.py` (a case for `/install.sh`)
- Modify: `.github/workflows/site.yml` only if the existing `unittest discover` step doesn't pick the new test up (it should)

**Interfaces:**
- Consumes: the installer file name from Task 1 (`irsync-installer.sh`).
- Produces: `sh install.sh [--beta|--alpha] [installer args...]`. Test-only environment: `IRSYNC_INSTALL_RELEASES_FILE` (read the releases JSON from this file instead of the API) and `IRSYNC_INSTALL_DRY_RUN=1` (print `would install <tag> <url> <args>` and exit 0 instead of downloading).

- [ ] **Step 1: Write the failing tests** in `site/tools/test_install.py`

Run `sh site/install.sh` via `subprocess`, with a fixture file of releases JSON shaped like the GitHub API (`[{"tag_name": "...", ...}, ...]`).

| test | releases | args | expect |
|---|---|---|---|
| `test_default_picks_latest_stable` | v0.2.1, v0.3.0-beta.2, v0.2.0 | – | `would install v0.2.1` |
| `test_beta_newer_than_stable` | v0.2.1, v0.3.0-beta.2, v0.3.0-alpha.5 | `--beta` | `v0.3.0-beta.2` |
| `test_beta_falls_back_when_stable_is_newer` | v0.3.0, v0.3.0-beta.2 | `--beta` | `v0.3.0` |
| `test_alpha_takes_newest_of_all` | v0.2.1, v0.3.0-beta.1, v0.3.0-alpha.7 | `--alpha` | `v0.3.0-beta.1` |
| `test_alpha_newer_than_beta` | v0.2.1, v0.3.0-beta.1, v0.4.0-alpha.1 | `--alpha` | `v0.4.0-alpha.1` |
| `test_numeric_ordering` | v0.9.0, v0.10.0, v0.10.0-beta.9, v0.10.0-beta.10 | – / `--beta` | `v0.10.0` / `v0.10.0` |
| `test_beta_number_ordering` | v0.1.0-beta.9, v0.1.0-beta.10 | `--beta` | `v0.1.0-beta.10` |
| `test_no_stable_release_yet` | v0.1.0-alpha.1 | – | exit ≠ 0, stderr mentions `--alpha` |
| `test_args_pass_through` | v0.2.1 | `--no-modify-path` | output ends with `--no-modify-path` |
| `test_url` | v0.2.1 | – | URL is `https://github.com/irsync-dev/irsync/releases/download/v0.2.1/irsync-installer.sh` |
| `test_github_unreachable` | `IRSYNC_INSTALL_RELEASES_FILE` names a missing file | – | exit ≠ 0, stderr says GitHub could not be reached and mentions the rate limit |
| `test_ignores_unknown_tag_shapes` | v0.2.1, `nightly`, v0.3.0-rc.1 | `--alpha` | `v0.2.1` |

Also: `test_posix_sh` runs `sh -n` on the script, and runs it under `dash` if installed.

- [ ] **Step 2: Run them, expect FAIL**

Run: `python3 -m unittest discover -s site/tools -p 'test_install.py'`

- [ ] **Step 3: Write `site/install.sh`**

POSIX sh, `set -eu`, no `jq`; needs `curl` or `wget`. Releases come from `https://api.github.com/repos/irsync-dev/irsync/releases?per_page=100`. Extract `tag_name` values with `grep`/`sed`. Accept only tags matching `vX.Y.Z`, `vX.Y.Z-beta.N` and `vX.Y.Z-alpha.N`. Compare versions as `(X, Y, Z, rank, N)`, where rank is alpha=0 < beta=1 < stable=2. Pick the highest tag allowed by the channel: stable only; stable or beta; or any. Print `irsync: installing <tag>` to stderr, then download that tag's installer and run it with the remaining arguments. If the API fails (HTTP error, 403 rate limit, no network), print an error naming the cause and the release page URL, and exit 1.

- [ ] **Step 4: Run the tests, expect PASS**

Run: `python3 -m unittest discover -s site/tools -p 'test_*.py'`
Expected: all pass, including the existing site tool tests.

- [ ] **Step 5: Check the site build includes the script**

Run: `site/build.sh && test -f target/site/install.sh && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: file present; check passes.

- [ ] **Step 6: Commit**

```bash
git add site/install.sh site/build.sh site/_headers site/tools/test_install.py site/tools/test_headers.py
git commit -m "Serve install.sh from irsync.org" -m "It picks the newest stable release, or a newer beta or alpha on request, and runs that release's installer, so it never goes stale."
```

---

### Task 5: Package and repository scripts

**Files:**
- Create: `packaging/nfpm.yaml`
- Create: `packaging/build-packages.sh`
- Create: `packaging/update-repo.sh`
- Create: `packaging/repo/index.html`, `packaging/repo/irsync.repo`, `packaging/repo/CNAME` (`dist.irsync.org`)
- Create: `packaging/tests/test_packaging.py`

**Interfaces:**
- Produces: `packaging/build-packages.sh <archives-dir> <version> <out-dir>`. For each `irsync-{x86_64,aarch64}-unknown-linux-musl.tar.xz` in `<archives-dir>`, writes `irsync_<version>_{amd64,arm64}.deb` and `irsync-<version>-1.{x86_64,aarch64}.rpm` to `<out-dir>`.
- Produces: `packaging/update-repo.sh <repo-checkout> <packages-dir> <keep>`. Adds the packages, keeps the newest `<keep>` versions per arch and format, and regenerates and signs all metadata. Signs with the key in `$GPG_KEY_ID`, using whatever `GNUPGHOME` is set; the passphrase comes from gpg-agent or `$GPG_PASSPHRASE`. Idempotent.
- Repo layout, as in the spec: `apt/dists/stable/{Release,InRelease,Release.gpg}`, `apt/dists/stable/main/binary-{amd64,arm64}/Packages{,.gz}`, `apt/pool/main/i/irsync/*.deb`, `rpm/{x86_64,aarch64}/*.rpm` + `repodata/` + `repodata/repomd.xml.asc`, `irsync.asc` (armoured public key), `irsync.gpg` (binary keyring for apt `signed-by`), `irsync.repo`, `index.html`, `CNAME`.

`nfpm.yaml`: name `irsync`; version, arch and binary directory from environment variables; maintainer `irsync releases <releases@irsync.org>`; licence MIT; homepage `https://irsync.org`; description from the README; contents `irsync` → `/usr/bin/irsync`, `fileset` → `/usr/bin/fileset`, both mode 0755.

`irsync.repo`: `[irsync]`, `baseurl=https://dist.irsync.org/rpm/$basearch`, `gpgcheck=1`, `repo_gpgcheck=1`, `gpgkey=https://dist.irsync.org/irsync.asc`. `index.html` shows the apt setup (fetch `irsync.gpg` into `/etc/apt/keyrings/`, a `deb [signed-by=...] https://dist.irsync.org/apt stable main` line) and the dnf setup (fetch `irsync.repo` into `/etc/yum.repos.d/`).

- [ ] **Step 1: Write the failing tests** in `packaging/tests/test_packaging.py`

Setup: a temp `GNUPGHOME` with a throwaway passphrase-less key for `irsync releases <releases@irsync.org>`. Build fake archives that contain shell-script stand-ins for `irsync` and `fileset`, named like the real ones. If `nfpm`, `apt-ftparchive`, `createrepo_c`, `rpm` or `gpg` is missing, skip with the name of the missing tool. When `PACKAGING_TESTS_REQUIRED=1`, fail instead of skipping.

- `test_build_packages_contents`: `dpkg-deb -c` lists exactly `./usr/bin/irsync` and `./usr/bin/fileset`; `dpkg-deb -f` shows `Version: 0.2.0`, `Architecture: amd64`/`arm64`, `Maintainer: irsync releases <releases@irsync.org>`; `rpm -qpl` lists `/usr/bin/irsync` and `/usr/bin/fileset`; `rpm -qp --qf '%{ARCH}'` gives `x86_64`/`aarch64`.
- `test_apt_metadata_signed`: after `update-repo.sh`, `gpg --verify apt/dists/stable/InRelease` and `gpg --verify Release.gpg Release` succeed. `binary-amd64/Packages` names `irsync` version `0.2.0` and its filename exists under `apt/pool`.
- `test_rpm_metadata_signed`: `gpg --verify rpm/x86_64/repodata/repomd.xml.asc` succeeds. `rpm --dbpath <tmp> --import irsync.asc` then `rpm --dbpath <tmp> -K` on the package reports the signature OK.
- `test_prunes_by_version`: add 0.8.0, 0.9.0, 0.10.0, 0.11.0 with keep=3. Remaining: 0.9.0, 0.10.0, 0.11.0 in both apt and rpm, for both arches, and the metadata lists only those.
- `test_rerun_same_version_is_idempotent`: run `update-repo.sh` twice with 0.2.0. Exactly one `.deb` per arch and one `.rpm` per arch; `Packages` lists 0.2.0 once; signatures still verify.
- `test_static_files`: `CNAME` is `dist.irsync.org`; `irsync.asc` and `irsync.gpg` hold the signing key's fingerprint; `irsync.repo` and `index.html` are present.

- [ ] **Step 2: Run them, expect FAIL**

Run: `PACKAGING_TESTS_REQUIRED=1 python3 -m unittest discover -s packaging/tests`
(Install `nfpm`, `apt-utils`, `createrepo-c`, `rpm`, `gnupg` first, or run inside an `ubuntu:24.04` container.)

- [ ] **Step 3: Write `nfpm.yaml`, the two scripts and the static repo files**

Sort versions with `sort -V`, or `dpkg --compare-versions` for Debian. Never sort lexically or by mtime. `update-repo.sh` signs `.rpm` files with `rpmsign --addsign` before running `createrepo_c`. It writes `InRelease` with `gpg --clearsign` and `Release.gpg` with `gpg --armor --detach-sign`.

- [ ] **Step 4: Run them, expect PASS**

Run: `PACKAGING_TESTS_REQUIRED=1 python3 -m unittest discover -s packaging/tests`

- [ ] **Step 5: Commit**

```bash
git add packaging
git commit -m "Add scripts that build .deb/.rpm packages and the signed apt/dnf repo" -m "Kept separate from the workflow so they can be tested locally with a throwaway key."
```

---

### Task 6: The packages publish job

**Files:**
- Create: `.github/workflows/packages.yml`
- Modify: `dist-workspace.toml` (`publish-jobs = ["homebrew", "./packages"]`), then `dist generate`
- Modify: `.github/workflows/release.yml` (regenerated)

**Interfaces:**
- Consumes: dist's `plan` input (JSON); Task 5's two scripts; the archives from Task 1.
- Secrets, by these names: `PACKAGES_REPO_TOKEN` (can push to `irsync-dev/packages`), `PACKAGES_GPG_PRIVATE_KEY`, `PACKAGES_GPG_PASSPHRASE`. The tap uses dist's `HOMEBREW_TAP_TOKEN`.

- [ ] **Step 1: Write `packages.yml`** (`on: workflow_call` with input `plan`, plus the secrets above)

One job on `ubuntu-latest`, with `concurrency: { group: packages-repo, cancel-in-progress: false }`:
1. Exit successfully at once if the plan JSON marks the release a pre-release (`announcement_is_prerelease`).
2. Read the version and tag from the plan.
3. Download the two Linux archives from this run's artifacts (`actions/download-artifact`, dist's `artifacts-*` pattern, merged).
4. Install `nfpm`, `apt-utils`, `createrepo-c`, `rpm`, `gnupg`.
5. Run `PACKAGING_TESTS_REQUIRED=1 python3 -m unittest discover -s packaging/tests`.
6. Run `build-packages.sh`.
7. Attach the `.deb`/`.rpm` files to the release: `gh release upload <tag> --clobber`.
8. Import the GPG key, check out `irsync-dev/packages` with `PACKAGES_REPO_TOKEN`, run `update-repo.sh <checkout> <out> 3`, then commit and push. The commit message is `irsync <version>`.

- [ ] **Step 2: Wire it in and regenerate**

Run: `dist generate`, then `dist generate --check` (no diff).
Expected: `release.yml` has a `custom-packages` job that calls `./.github/workflows/packages.yml`, after `host` and before `announce`, with `secrets: inherit`. If dist doesn't pass secrets through, use the same approach as Task 3 Step 2's fallback and note it in `releasing.md`.

- [ ] **Step 3: Lint the workflows**

Run: `actionlint .github/workflows/*.yml` (install it if missing).
Expected: no errors.

- [ ] **Step 4: Commit**

```bash
git add .github/workflows/packages.yml .github/workflows/release.yml dist-workspace.toml
git commit -m "Publish stable releases to the apt and dnf repositories" -m "Pre-releases stop at the GitHub Release."
```

---

### Task 7: Docs

**Files:**
- Modify: `README.md` (install section; platform line; fileset install)
- Modify: `book/src/introduction.md` (same)
- Modify: `book/src/fileset/introduction.md` (install: same packages as irsync)
- Create: `book/src/contributing/releasing.md`, and add it to `book/src/SUMMARY.md`
- Modify: `AGENTS.md` (one line: releases → `book/src/contributing/releasing.md`)

- [ ] **Step 1: User-facing install docs**

Replace "Release binaries will be provided once the beta starts." with, in this order:
- the installer: `curl --proto '=https' --tlsv1.2 -LsSf https://irsync.org/install.sh | sh`, plus `sh -s -- --beta`;
- Homebrew: `brew install irsync-dev/tap/irsync`;
- apt and dnf: link to `https://dist.irsync.org`;
- tarballs from the GitHub Releases page;
- `cargo install --git ...` for everything else.

Say every package contains both `irsync` and `fileset`. Platform line: release binaries cover Linux x86_64 and arm64 and macOS on Apple Silicon. Other Linux and macOS machines can build from source.

- [ ] **Step 2: `releasing.md`**

Sections:
- **Cutting a release:** bump `[workspace.package] version` *and* `packaging/dist/dist.toml`, commit, push to `main`, wait for green CI, then `git tag vX.Y.Z && git push origin vX.Y.Z`. Explain pre-release tags.
- **What the workflow does:** the five steps from the spec's release flow.
- **If a release fails:** gate failed means nothing was published; fix, then retag with a new patch version. Packages job failed means rerun that job, since it's idempotent.
- **One-time setup:** the spec's list, with secret names from Task 6.
- **Rotating the signing key.**
- Any hand-patches from Tasks 3 and 6.

- [ ] **Step 3: Check the site**

Run: `site/build.sh && python3 site/tools/check.py --irsync target/debug/irsync target/site`
Expected: passes. The flag check may flag `--beta`/`--alpha` as unknown irsync flags; if so, word or mark them the way the checker allows for non-irsync commands.

- [ ] **Step 4: Commit**

```bash
git add README.md book AGENTS.md
git commit -m "Document installing releases and cutting one"
```

---

### Task 8: Full regression, then hand over to the maintainer

- [ ] **Step 1: Full regression** (per `AGENTS.md`)

```sh
touch crates/irsync/build.rs
cargo test -q --workspace --all-features --no-fail-fast -- --include-ignored
cargo fmt --all --check
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo clippy --workspace --all-targets --no-default-features -- -D warnings
python3 -m unittest discover -s site/tools -p 'test_*.py'
PACKAGING_TESTS_REQUIRED=1 python3 -m unittest discover -s packaging/tests
dist plan
```

Expected: all pass, 0 failed.

- [ ] **Step 2: Stop and hand over.** The remaining steps need the maintainer's accounts and must not be done by the implementer:
  - create `irsync-dev/homebrew-tap` and `irsync-dev/packages`;
  - set up Pages and the `dist.irsync.org` DNS record;
  - create the GPG key and the token, and add the four secrets;
  - set up the `releases@irsync.org` mailbox;
  - push a rehearsal tag `v0.1.0-alpha.1`, which exercises the gate, the builds and the GitHub Release but skips Homebrew, apt and dnf;
  - then cut the first stable release.
