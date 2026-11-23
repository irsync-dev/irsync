# Supply chain security

irsync runs on machines that hold people's data, often as a daemon, so a
backdoored irsync would be a backdoor into all of them. This page lists what
guards the way from a commit to a user's install, and the steps only a
maintainer can do. Why each choice was made is in
[SUPPLY_CHAIN_DECISIONS.md](https://github.com/irsync-dev/irsync/blob/main/docs/SUPPLY_CHAIN_DECISIONS.md).

## What guards what

**Someone takes over the repository.**

- Two-factor login is required for the `irsync-dev` organisation.
- `main` only changes through a pull request that passed the `CI result`
  check. Nobody can push to it directly or force-push, admins included. A
  pull request needs no approving review while irsync has one maintainer.
- Release tags (`v*`) are created by GitHub when a maintainer publishes the
  draft release, not by a push. They can never be moved or deleted. Anyone with write
  access can still push a new `v*` tag by hand. That is caught, not
  blocked: a tag with no release stops the next gate run, and
  `tools/github_settings.py check` reports it.
- `release-*` branches take changes only through a pull request that passed
  `CI result`, like `main`. The `pinned/*` branches the gate makes can't be
  moved.
- The signing key and the tokens for the packages repo and the Homebrew tap
  live in the `release` environment. It opens only for jobs running on a
  `v*` tag, and only after a maintainer approves. A change merged to `main`
  can't reach them. No secret is stored repo-wide; the settings check
  reports one if it appears.
- The Cloudflare tokens live in the `site` environment, which opens only
  for runs on `main`.
- Every Action is pinned to a commit, not a version tag its owner could
  move. `.github/scripts/tests/test_workflow_policy.py` fails CI when a
  workflow breaks one of these rules.
- Secret scanning and push protection catch a secret before it's pushed.

**A dependency goes bad.**

- `cargo deny --locked check` runs on every change (`deny.toml`). It fails on:
  - any RustSec advisory, including unmaintained or yanked crates;
  - a license that isn't on the list;
  - a crate from anywhere but crates.io and the pinned upstream iroh commit;
  - a second version of a crate that isn't listed as accepted (see
    [Accepting a second version of a crate](#accepting-a-second-version-of-a-crate)).

  An exception in `deny.toml` that no longer matches anything also fails
  the check, so a stale one gets removed.

  It looks at the crates built for the platforms irsync and its tools are
  built for, listed under `targets` in `deny.toml`. Both Windows targets are
  there, because `filegen` and `fileset` are built for them, so the check
  covers every crate only a Windows build uses, those of the whole workspace
  and iroh's too. A crate built only for a platform not listed (illumos, say)
  is left out. Add a platform there when something of ours starts building
  for it.

  Two exceptions are in `deny.toml` today. One advisory is accepted:
  RUSTSEC-2026-0543, because `chrono` is unmaintained. It is built on
  Windows only, through `netwatch` (iroh); see
  [issue 174](https://github.com/irsync-dev/irsync/issues/174). One duplicate
  is skipped: `nix` 0.31, which `ctrlc` 3.5.2 asks for while everything else
  is on 0.32. It goes when `ctrlc` moves to 3.5.3, which is not before
  2026-11-25. The reasons are in `deny.toml` and in
  [Accepting an advisory](#accepting-an-advisory) and
  [Accepting a second version of a crate](#accepting-a-second-version-of-a-crate).
- The licenses of every crate in `Cargo.lock`, for every platform, are
  checked too (`deny-license-check.toml`, the `deny-license-check` job). A
  failure there shows on a pull request without blocking it, but it stops
  a release. The two files must allow the same licenses; a test checks that.
- The same advisory check runs daily (`advisories.yml`), so a new advisory
  against untouched code is noticed.
- Dependency review flags a pull request that adds a vulnerable crate.
- Dependabot proposes updates weekly, but only once a release is 7 days old.
  Most hijacked releases are pulled within days. Security fixes come
  without the wait.

**Someone swaps a binary.**

- Every archive, installer and package gets an attestation. Users can check
  one with `gh attestation verify`, see
  [Verify a download](../how-to/verify-a-download.md).
- Every file on a release is signed with a minisign key that is kept
  offline, on a USB stick, and never goes into GitHub. A maintainer signs
  the draft on their own computer and then publishes it
  (`.github/scripts/sign-release.sh`, see
  [Releasing](releasing.md#signing-the-release)). Someone who takes over
  the organisation, a token or a workflow can build a release, but can't
  make a signature that passes. Users check it as in
  [Verify a download](../how-to/verify-a-download.md). The one-off key setup
  is in [The release signing key](release-signing-key.md).
- Releases are immutable once published: no file can be replaced or added.
- `install.sh` refuses plain HTTP, even after a redirect.
- The apt/dnf key's primary part is kept offline. GitHub holds only a
  signing subkey, which can be revoked and replaced without users trusting
  a new key.

**A bug slips in.**

- Fuzzing feeds the code that reads bytes from a peer millions of made-up
  inputs and watches for a crash. The seeds are checked on every pull
  request, and each target is fuzzed for ten minutes every night
  (`nightly.yml`). A crash fails the run and uploads the input, which is
  public, as the Fuzzing section of [Testing](testing.md#fuzzing) explains.
- Every `unsafe` block needs a `// SAFETY:` comment, and clippy refuses one
  without. Miri runs the tests it can run every night, and stops at
  undefined behaviour (memory use the language does not allow).
- zizmor checks the workflows for security problems, in the `workflows`
  job. CodeQL, GitHub's code analysis, reads the Rust code and the
  workflows, and shows what it finds in the Security tab. It is not a
  required check until it has been quiet for a few weeks. See
  [Static analysis](testing.md#static-analysis).

**Someone finds a vulnerability.**

- There are two ways to report it: the **Report a vulnerability** button on
  the repository's Security tab (private vulnerability reporting), or an
  email to `security@irsync.org`. [SECURITY.md](https://github.com/irsync-dev/irsync/blob/main/SECURITY.md)
  says so.
- A report is answered within 7 days.
- `tools/github_settings.py check` reports the setting if it is turned off.

## The Scorecard

The [OpenSSF Scorecard](https://scorecard.dev) grades a public repository on
about 18 security habits, such as pinned Actions, a security policy and
fuzzing, and gives each a score from 0 to 10 and the project an overall
score out of 10. `scorecard.yml` runs it on every push to `main`, every
week, and when the branch protection settings change. The result is at
[scorecard.dev](https://scorecard.dev/viewer/?uri=github.com/irsync-dev/irsync),
the badge in the README links to it, and the findings also show in the
Security tab.

The score describes the project as it is. We do not change anything only to
raise it. At the time of writing (2026-11-20) it is 6.0 out of 10. Most
of the gap is in the checks below, which we accept or which rise with time.
Every check not listed here scores 10 (or is marked "not applicable", as
Packaging is: irsync has no package registry).

| Check | Score | Why it is low | What we do |
| --- | --- | --- | --- |
| Maintained | 0 | The repository is younger than 90 days. | Rises by itself in February 2027. |
| Code-Review | 0 | No change has been approved by a second person, because irsync has one maintainer. | Accepted. Changes when more maintainers join (see below). |
| Branch-Protection | 3 | `main` needs a pull request and a passing `CI result`, but no approving review (one maintainer). It also does not require a branch to be up to date before merging, which would mean a rebase for every pull request. | Accepted. The approval part changes when more maintainers join. |
| Contributors | 0 | All contributors belong to one organisation. | Accepted. We can't change this. |
| CII-Best-Practices | 0 | irsync has no OpenSSF Best Practices badge. It is a form the maintainer fills in. | Later. |
| Signed-Releases | 3 | Scorecard looks at the last five releases. Only the two newest are signed, and none has provenance (a signed record of how it was built). | Rises with each signed release. The old ones can't be changed. |
| Token-Permissions | 0 | `release.yml` is written by dist and has `contents: write` at the top. The release jobs also need their write permissions to publish. Scorecard scores any such write as a failure. | Accepted. |
| Pinned-Dependencies | 8 | Three downloads are piped into a shell without a hash: the dist installer in `release.yml` (two places) and the rustup install in `tools/dev-setup.sh`. Every Action is pinned. | Accepted. The first two are written by dist, the third is a developer setup script. |
| Vulnerabilities | 9 | Scorecard reads every entry in `Cargo.lock`, also for crates that are not built for the platforms irsync ships on. One advisory is left: `chrono` is unmaintained. It is built on Windows only, through iroh. `cargo deny` also checks the Windows targets now, so it sees `chrono`; it passes because `deny.toml` accepts that one advisory, with the reason, and nothing else. | We asked upstream to drop it. See [issue 174](https://github.com/irsync-dev/irsync/issues/174). |

The scores move when the repository changes and when Scorecard changes its
checks, so read the live result for the current numbers.

## Checking the settings

`tools/github_settings.py` holds the settings for `irsync-dev/irsync`,
`irsync-dev/iroh`, `irsync-dev/homebrew-tap` and `irsync-dev/packages`.
These are the rulesets, environments, secret scanning, SHA pinning and
immutable releases and private vulnerability reporting.

```sh
tools/github_settings.py check    # prints every difference, changes nothing
tools/github_settings.py apply    # sets them; needs an org admin
```

`apply` creates the `main` ruleset only once the checked-out commit has a
passing `CI result` check. Otherwise every pull request would wait on a
check that never reports. Run `check` now and then, and after changing a
setting by hand.

## Accepting an advisory

When `cargo deny` fails on an advisory, first try to upgrade:
`cargo update -p <crate>`, or bump the version in `Cargo.toml`. If you
can't yet, accept it in `deny.toml` under `[advisories]`, with the ID and
the real reason:

```toml
ignore = [
    { id = "RUSTSEC-2026-0000", reason = "no fixed version yet; irsync never calls foo()" },
]
```

Remove the entry once a fix is in. A reason like "not used" without saying
why is not a reason.

## Accepting a second version of a crate

Each copy of a crate is more code to build, ship and keep an eye on. So
`cargo deny` fails when a crate is in the tree in more than one version,
unless `deny.toml` lists that version under `skip` in `[bans]`. A routine
`cargo update` or a new dependency can set this off.

First see who pulls in each version:

```sh
cargo tree -i aes-gcm@0.10 --all-features -e normal,build,dev
```

If one of our own crates or a direct dependency is behind, update it. If the
older version comes from a crate we don't control, accept it: add the older
version to `skip`, with the crate that pulls it in as the reason:

```toml
skip = [
    { crate = "aes-gcm@0.10", reason = "noq-proto (iroh) uses aes-gcm 0.10" },
]
```

Write the version without the last part (`0.10`, or `2` for `2.x`), so a
patch release doesn't need a change here. Each version gets its own entry.
`skip-tree`, which accepts a crate and everything below it, would also hide
the next duplicate in that part of the tree.

When upstream catches up and the older version is gone, the entry no longer
matches and the check fails until you remove it.

## Maintainer steps

These need a person. Do them in order. Secret values can't be read back
from GitHub, so each one is entered fresh.

**1. Two-factor login.** Add a passkey or a hardware key to your GitHub
account (Settings, Password and authentication), not SMS. Then, in the
`irsync-dev` organisation, go to Settings, Authentication security, and
tick "Require two-factor authentication".

**Dependency graph.** In `irsync-dev/irsync`, go to Settings, Code
security, and enable "Dependency graph". The dependency review job in CI
needs it, and there is no API to turn it on.

**2. Split the signing key.** The primary key stays offline. GitHub gets only
a signing subkey. Use the separate keyring from
[Making the signing key](releasing.md#making-the-signing-key):

```sh
export GNUPGHOME="$PWD/gnupg"
FPR=$(gpg --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')
gpg --batch --pinentry-mode loopback --passphrase-file passphrase.txt \
    --quick-add-key "$FPR" rsa4096 sign never
SUB=$(gpg --list-secret-keys --with-colons "$FPR" | awk -F: '$1 == "fpr" { n++ } n == 2 { print $10; exit }')
gpg --batch --pinentry-mode loopback --passphrase-file passphrase.txt \
    --armor --export-secret-subkeys "$SUB"'!' > private-subkey.asc
gpg --armor --export "$FPR" > public-key.asc
```

The `!` exports exactly that subkey. It sits in single quotes so that
zsh and bash don't read it as history expansion. `private-subkey.asc` goes into GitHub.
Keep `private-key.asc`, `passphrase.txt` and the revocation certificate
offline. Users keep trusting the primary fingerprint, which doesn't change.

The subkey never expires, like the key itself: apt users keep the public key
in a local file that nothing refreshes, so an expiry date would break
`apt update` for all of them on that day.

If the subkey leaks, revoke it in this keyring (`gpg --edit-key "$FPR"`,
`key 1`, `revkey`, `save`), add a new one as above, and replace the secret.
Then commit the re-exported `public-key.asc` to the packages repo as
`irsync.asc` and `irsync.gpg`, as in rotation step 3. The fingerprint users
trust stays the same, but apt and dnf only learn the new subkey when they
download the key file again, so tell users to (rotation step 4).

**3. Tokens.** Make two fine-grained personal access tokens, each limited to
one repository with "Contents: read and write" and nothing else:

| Token | Repository |
| --- | --- |
| for `HOMEBREW_TAP_TOKEN` | `irsync-dev/homebrew-tap` |
| for `PACKAGES_REPO_TOKEN` | `irsync-dev/packages` |

**4. Environments and their secrets.** Run `tools/github_settings.py apply`
first: it creates the `release` and `site` environments. Then:

```sh
gh secret set --env release HOMEBREW_TAP_TOKEN
gh secret set --env release PACKAGES_REPO_TOKEN
gh secret set --env release PACKAGES_GPG_PRIVATE_KEY < private-subkey.asc
gh secret set --env release PACKAGES_GPG_PASSPHRASE < passphrase.txt
gh secret set --env site CLOUDFLARE_API_TOKEN_DEV
gh secret set --env site CLOUDFLARE_API_TOKEN_PUB
gh secret set --env site CLOUDFLARE_ACCOUNT_ID
```

`gh secret set` asks for a value when nothing is piped in.

**5. Delete the old repository-wide secrets**, so only the environments hold
them:

```sh
for s in HOMEBREW_TAP_TOKEN PACKAGES_REPO_TOKEN PACKAGES_GPG_PRIVATE_KEY \
         PACKAGES_GPG_PASSPHRASE CLOUDFLARE_API_TOKEN_DEV CLOUDFLARE_API_TOKEN_PUB \
         CLOUDFLARE_ACCOUNT_ID; do gh secret delete "$s"; done
gh secret list    # prints nothing
```

Before the first stable release, check the key and tokens with a resume dry
run on a pre-release. See
[Finishing a release with the resume workflow](releasing.md#finishing-a-release-with-the-resume-workflow).

## When more maintainers join

These are settings changes in `tools/github_settings.py`, not a redesign:

- the `main` ruleset requires one approving review;
- add a `CODEOWNERS` file so changes to `.github/`, `packaging/`,
  `dist-workspace.toml`, `deny.toml` and `site/install.sh` need a
  maintainer's review;
- the `release` environment gets `prevent_self_review`, so whoever starts a
  release can't also approve it. This is not enough alone: the resume
  run is started by `github-actions[bot]`, not by the person who started
  the gate, so the setting would not stop that person from approving
  their own release.
