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
- Release tags (`v*`) can be pushed but never moved or deleted. A bad tag
  means a new version.
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
  - a crate from anywhere but crates.io and our iroh fork.
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
- Releases are immutable once published: no file can be replaced or added.
- `install.sh` refuses plain HTTP, even after a redirect.
- The apt/dnf key's primary part is kept offline. GitHub holds only a
  signing subkey, which can be revoked and replaced without users trusting
  a new key.

## Checking the settings

`tools/github_settings.py` holds the settings for `irsync-dev/irsync`,
`irsync-dev/iroh`, `irsync-dev/homebrew-tap` and `irsync-dev/packages`.
These are the rulesets, environments, secret scanning, SHA pinning and
immutable releases.

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
- the `release` environment gets `prevent_self_review`, so whoever tags a
  release can't also approve it.
