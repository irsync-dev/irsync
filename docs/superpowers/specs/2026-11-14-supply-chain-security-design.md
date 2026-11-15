# Supply chain security for irsync

Date: 2026-11-14. Status: reviewed, awaiting the maintainer's approval.

## Why

irsync is a file sync tool. People run it on machines that hold their data,
often as a daemon. A backdoored irsync would be a backdoor into all of them.
This work closes the ways someone could get bad code into what users install,
using what GitHub offers.

The maintainer named three threats:

1. **Repo takeover.** Someone gets control of `irsync-dev/irsync` (stolen
   login or token, a crafted pull request, a compromised Action) and changes
   the code or the release process.
2. **Bad dependencies.** A crate irsync links against, directly or through
   another crate, turns malicious, or a known vulnerable version stays in.
3. **Tampered binaries.** Users install something other than what CI built,
   whether through a man-in-the-middle or a takeover of a download source.

## Who works on this

Today one person maintains irsync and is the only admin of the `irsync-dev`
org. Later a small group of maintainers will join. Every control must work
for a solo maintainer now (no rule that needs a second person) and be
switchable to a group setup by changing settings, not by redesigning.

## Current state (checked 2026-11-14)

Already good, keep as is:

- The default workflow token is read-only, and workflows declare narrow
  `permissions` per job.
- No workflow uses `pull_request_target`. First-time contributors need
  approval before their PRs run CI.
- `Cargo.lock` is committed. CI uses `--locked`. The iroh fork is pinned to a
  commit (`irsync-dev/iroh?rev=28466cc…`).
- dist, actionlint and nfpm downloads in workflows are checked by SHA-256.
- apt and rpm repos (`dist.irsync.org`) are GPG-signed. `install-updater =
  false`, so no auto-update.
- All downloads users make go over HTTPS.
- Vulnerability alerts are on.

Gaps:

- `main` has no branch protection or ruleset.
- The `Release tags` ruleset (`refs/tags/v*`: creation, update, deletion,
  non-fast-forward) has an admin bypass set to `always`. With one admin, it
  stops nothing.
- The org does not require two-factor login.
- All seven secrets are repo-level. No environments exist. Secrets:
  `PACKAGES_GPG_PRIVATE_KEY`, `PACKAGES_GPG_PASSPHRASE`,
  `PACKAGES_REPO_TOKEN`, `HOMEBREW_TAP_TOKEN`, `CLOUDFLARE_API_TOKEN_DEV`,
  `CLOUDFLARE_API_TOKEN_PUB`, `CLOUDFLARE_ACCOUNT_ID`.
- Actions use movable version tags (`actions/checkout@v6`,
  `Swatinem/rust-cache@v2`, `taiki-e/install-action@v2`, `actions/setup-node@v6`,
  `actions/upload-artifact@v7`, `actions/download-artifact@v8`). The Actions
  setting allows all actions and does not require SHA pinning.
- Secret scanning and push protection are off.
- Dependabot security updates are off. No `dependabot.yml`. No `cargo-deny`
  or `cargo-audit`.
- No build attestations. Immutable releases are off.
- `site/install.sh` downloads with plain `curl -fsSL` / `wget -qO-`, so a
  redirect could drop to plain HTTP.

## Goals

### 1. Repo takeover

Now (solo):

- **Two-factor login required** for the `irsync-dev` org. The maintainer uses
  a passkey or hardware key, not SMS. (The maintainer does this; the builder
  can turn on the org setting.)
- **Ruleset on `main`**, no bypass for anyone, admins included:
  - changes only through a pull request, with 0 required approvals (so the
    solo maintainer can merge their own PRs)
  - required status checks: the CI jobs from `ci.yml` and the new
    `cargo-deny` check. `ci.yml` skips Markdown-, `book/`- and `site/`-only
    changes via `paths-ignore`, so such a PR would never report the
    required checks and could never merge. The builder fixes this (for
    example a job that always runs and reports success when the real checks
    are skipped).
  - no force-push, no deletion
- **Tag ruleset changed**: creating `v*` tags allowed, update and deletion
  blocked, no bypass for anyone. A published tag can never move.
- **`release` environment** holding `PACKAGES_GPG_PRIVATE_KEY`,
  `PACKAGES_GPG_PASSPHRASE`, `PACKAGES_REPO_TOKEN` and `HOMEBREW_TAP_TOKEN`.
  Deployments only from `v*` tags, never from a branch. Required reviewer:
  the maintainer (self-review allowed for now). The repo-level copies are
  deleted.
  - GitHub asks for approval once per workflow run that uses an
    environment. A stable release has two such runs: the dispatched
    `release.yml` run (Homebrew) and the `release-gate.yml` run
    (packages). **Two approval clicks per release is accepted** by the
    maintainer.
  - dist can't put its built-in Homebrew publish job in an environment.
    **Replace it with our own publish job** that names the `release`
    environment, wired in through dist's custom publish-jobs config (so
    `release.yml` stays generated, not hand-edited). Drop `"homebrew"` from
    dist's `publish-jobs`; dist still builds the formula file.
  - **`release-resume.yml` runs from the tag, not from `main`.** Its
    "Check the branch" guard changes to check that it's running on a
    `refs/tags/v*` ref matching its `tag` input (or the input goes away
    and the ref is the tag). This keeps "secrets only open from a tag"
    true for every path, including resume dry runs.
- **`site` environment** holding `CLOUDFLARE_API_TOKEN_PUB` and
  `CLOUDFLARE_ACCOUNT_ID`, deployments only from `main`. Whether
  `CLOUDFLARE_API_TOKEN_DEV` also moves there, or to its own environment, is
  the builder's call based on what it deploys.
- **Every Action pinned to a full commit SHA**, with the version in a comment.
  Turn on the repo setting that requires SHA pinning. Dependabot keeps the
  pins updated.
- **Secret scanning and push protection on.**
- **Same account and branch rules for the other repos that feed users:**
  `irsync-dev/iroh` (linked into irsync), `irsync-dev/homebrew-tap` and
  `irsync-dev/packages`. In the iroh fork, the commit irsync pins lives on
  branch `irsync-1.3.0`, not `main`. Protect `irsync-*` branches from
  deletion and force-push; a newer upstream version gets a new `irsync-*`
  branch, so rebasing onto upstream never needs a force-push (Claude's
  call, from the branch naming). On the tap and packages repos the release workflow
  pushes directly, so their rules must let that token push while blocking
  force-push and deletion. The builder works out the exact shape.

Later (group), by settings only:

- Require 1 approving review on `main`.
- Add `CODEOWNERS` so `.github/`, `packaging/`, `dist-workspace.toml`,
  `site/install.sh` and `deny.toml` need the maintainer's review.
- `release` environment: prevent self-review.

### 2. Dependencies

- **`cargo-deny` in CI**, as a required check, with a committed `deny.toml`:
  - `advisories`: any RustSec advisory **fails** the build. An accepted
    advisory goes in `ignore` with its ID and a written reason. Unmaintained
    and yanked crates also fail, with the same exception route.
  - `licenses`: an allow-list matching what the current tree uses, provided
    each is compatible with irsync's MIT license. The builder lists them from
    the current tree and flags anything odd to the maintainer.
  - `sources`: only crates.io, plus `https://github.com/irsync-dev/iroh`.
    Anything else fails.
  - `bans`: duplicate versions warn, nothing is banned yet.
- **Daily scheduled run** of the `cargo-deny` advisories check on `main`, so
  a new advisory against unchanged code is noticed. A failure shows up as a
  failed workflow run (GitHub emails the maintainer).
- **GitHub dependency review** (`actions/dependency-review-action`) on pull
  requests, failing on new dependencies with known vulnerabilities.
- **Dependabot** (`.github/dependabot.yml`):
  - `cargo` and `github-actions` ecosystems, weekly, updates grouped into one
    PR per ecosystem
  - a cooldown of 7 days before a new version is proposed
  - Dependabot security updates turned on in repo settings; these are not
    delayed by the cooldown

### 3. Tampered binaries

- **Build attestations**: set `github-attestations = true` in
  `dist-workspace.toml` and regenerate `release.yml` with dist. Every
  archive and installer gets a signed record of its commit and workflow.
  The `.deb` and `.rpm` files built in `packages.yml` get attestations too
  (`actions/attest-build-provenance`).
- **Immutable releases** turned on for the repo, so release files can't be
  replaced or deleted after publishing. Release notes stay editable, which
  `release-notes.yml` relies on.
  - This rules out adding files after publishing, which `packages.yml`
    does today (`gh release upload`, around line 167). **The `.deb` and
    `.rpm` files stop being attached to the GitHub release.** They live
    only on dist.irsync.org, each with an attestation. `packages.yml` drops
    the upload step, `release-resume.yml` drops its "attach missing files"
    role, and the book's install page and `releasing.md` say where the
    packages are.
- **`install.sh` hardening**: its own downloads use `curl --proto '=https'
  --tlsv1.2` and the wget equivalent (`--https-only`), so they refuse plain
  HTTP even after a redirect. `site/tools/test_install.py` keeps passing and
  gains a test for this.
- **Tokens narrowed**: `HOMEBREW_TAP_TOKEN` and `PACKAGES_REPO_TOKEN` are
  fine-grained tokens limited to their one repo with contents write only.
  The builder checks what they are now; the maintainer creates replacements
  if needed.
- **GPG key split**: the primary key moves offline (kept by the maintainer).
  GitHub holds only a signing subkey. Users keep trusting the same primary
  key, so a leaked subkey can be revoked and replaced without users
  re-importing a new key. The maintainer does the key work; the builder
  writes the steps.
- **Manual verification is documented, not enforced**: `install.sh` does not
  check attestations itself. A new how-to page in the book explains how to
  check a download with `gh attestation verify`, and how to check the apt and
  rpm key fingerprint.

### Docs

- `SECURITY.md` gets a short section on how releases are built and how to
  verify them, linking to the how-to page.
- `book/src/contributing/releasing.md` covers the two environment approval
  clicks, resume now running from the tag, the tag rule, and how to add a
  `deny.toml` exception. Its current recovery step "delete the tag and tag
  again" is removed: a tag can't be moved or deleted, so a bad tag means a
  new version.
- A design record in `docs/IRSYNC_DECISIONS.md` (or wherever this project
  keeps such records, see `book/src/contributing/design-records.md`) notes the
  choices made here and why.

## Non-goals

- `cargo-vet` (recording a review of every crate). Too much for one person
  with about 594 crates. Revisit once there's a group.
- `install.sh` refusing to install without a verified attestation.
- Signed commits required on `main`.
- Reproducible builds.
- Auto-update. It stays off.

## Anti-goals

- **Blocking the solo maintainer.** No rule may need a second person. If a
  setting would lock the maintainer out of merging or releasing, it's wrong.
- **A release process that breaks.** Releases go `release-gate.yml` →
  `release.yml` (dispatched) → `packages.yml` and `release-notes.yml`, plus
  `release-resume.yml` for retries. All of that must keep working, including
  the resume path and dry runs.
- **Hand edits dist overwrites.** `release.yml` is generated by dist. Changes
  to it go through dist config, or CI's dist check fails and the next
  `dist generate` loses them.
- **Noise.** Dependabot and checks should not produce a stream of PRs or red
  builds the maintainer learns to ignore.

## Constraints

- dist 0.34.0 generates `release.yml`. CI checks it is current
  (`ci.yml` around line 131). Pinning Actions in `release.yml` must use
  dist's own setting for this (dist supports pinning action commits in its
  config), not hand edits. If dist can't pin some action, note it and leave
  that one on its tag.
- The repo's working rules in `AGENTS.md` apply: the full regression before
  pushing to `main`, formatting checked by the pre-push hook, commit messages
  per the maintainer's style.
- Some steps need the maintainer: org 2FA enrollment, creating fine-grained
  tokens, the GPG key split, and approving the first release under the new
  environment. The plan groups these as clearly marked manual steps.
- Settings changes made through `gh api` should be recorded, as a script or
  a checklist in the repo, so they can be re-checked or re-applied later.

## Left to the builder

- Order of work, and how to split it into PRs. Settings that would block the
  builder's own PRs (the `main` ruleset requiring status checks) come last,
  once the checks they name exist.
- Exact `deny.toml` contents beyond the rules above.
- Workflow file names and job names for the new checks.
- How to test the release changes before a real release (dry runs, a
  pre-release tag, or both).
- Wording and placement of the how-to page.

## Decided after review

The builder check raised three conflicts. The maintainer took Claude's
recommendation on each:

1. Immutable releases vs. packages attached after publishing: packages move
   to dist.irsync.org only.
2. Resume ran from `main` vs. secrets only from tags: resume runs from the
   tag.
3. One approval per release was really two, and dist's Homebrew job can't
   use an environment: accept two clicks, replace the Homebrew job with our
   own.

## Calls made by Claude, not the maintainer

These came up in the conversation as proposals and were not objected to, or
were settled while writing this up:

- 7-day Dependabot cooldown, weekly grouped updates.
- Manual approval on each release via the `release` environment.
- Protecting `irsync-*` branches in the iroh fork.
- Duplicate crate versions warn rather than fail.
- The `.deb`/`.rpm` files get attestations too.
- A Cloudflare `site` environment.
- The same protections on `iroh`, `homebrew-tap` and `packages` repos.
- The docs section and the settings script/checklist.
