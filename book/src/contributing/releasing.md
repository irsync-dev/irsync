# Releasing

A release starts when you push a tag. The rest is done by GitHub Actions.
This page says how to cut a release, what the workflows do, what to do when
one fails, and what to set up once.

Only three kinds of tag release anything:

| Tag | What it is | Where it goes |
| --- | --- | --- |
| `vX.Y.Z` | A stable release | GitHub Release, Homebrew, apt, dnf |
| `vX.Y.Z-beta.N` | A beta | GitHub Release only |
| `vX.Y.Z-alpha.N` | An alpha | GitHub Release only |

Any other tag that starts with `v` and a digit stops with an error within
seconds, before the tests. Nothing is published.

## Cutting a release

1. On an up-to-date `main` with no uncommitted changes, run the bump script
   with the new version, without the `v`:

   ```sh
   .github/scripts/bump-version.sh 1.2.3
   ```

   It sets the version in `Cargo.toml` (`[workspace.package]`) and
   `packaging/dist/dist.toml`, and updates `Cargo.lock` to match. It commits
   the three files as `Release v1.2.3` on a new branch, `release-v1.2.3`.
   It doesn't push or tag. It prints the commands for the next steps.
2. Open a pull request. `main` only takes changes through one:

   ```sh
   git push -u origin release-v1.2.3
   gh pr create --fill
   ```

3. Wait for its `CI result` check to pass. That includes the `release build`
   jobs, which run the Linux release build the way the release will. Then
   merge it: `gh pr merge --merge`.
4. Tag the merged commit on `main` and push the tag. Use an annotated tag
   (`-a`). The tag can't be moved or deleted once pushed, so check that
   `main` holds the new version first:

   ```sh
   git switch main && git pull --ff-only
   git tag -a v1.2.3 -m "irsync 1.2.3"
   git push origin v1.2.3
   ```

For a pre-release, use a version such as `1.2.3-beta.1` or `1.2.3-alpha.1`,
and the tag `v1.2.3-beta.1`.

If you change the version by hand instead, keep all three files in step.
These checks catch a mismatch:

- A test (`crates/irsync/tests/release_version.rs`) fails if `Cargo.toml` and
  `dist.toml` differ.
- CI builds with `--locked`, so it fails if `Cargo.lock` is out of date. The
  release build uses `--locked` too, and would fail later.
- The release gate stops within seconds if the two files don't match the tag.

## What the workflow does

Pushing the tag starts `.github/workflows/release-gate.yml`. It is the only
way a release starts. It does this, in order:

1. **Check.** It checks the tag name, and that the version in `Cargo.toml`
   and `packaging/dist/dist.toml` is the tag without the `v`. This takes
   seconds.
2. **Test.** It runs `ci.yml` on the tagged commit, with the musl release
   targets added. If anything fails, it stops. Nothing is published.
3. **Release.** It checks that the tag still points at the commit that was
   tested. Then it launches `release.yml` for the tag
   and waits for it. `release.yml` builds `irsync` and `fileset` for Linux
   x86_64, Linux arm64 and macOS on Apple Silicon. It publishes the GitHub
   Release with the tarballs, checksums, the install script and the
   Homebrew formula, each with an attestation. The release is immutable from
   then on: no file on it can be replaced or added.
4. **Packages and Homebrew** (stable tags only). Both jobs wait for you to
   approve the `release` environment, which holds their secrets. Open the
   gate run and click "Review deployments", tick `release` and approve: one
   click lets both start.
   - `.github/workflows/packages.yml` builds the `.deb` and `.rpm` files
     from the released Linux tarballs. It signs the `.rpm` files, adds
     everything to `irsync-dev/packages`, signs the apt and dnf metadata and
     pushes. It keeps the newest 3 versions. GitHub Pages serves the result
     at `dist.irsync.org`. The packages get attestations, but are not
     attached to the release.
   - `.github/workflows/publish-homebrew.yml` pushes the formula from the
     release to `irsync-dev/homebrew-tap`.
5. **Notes.** It adds GitHub's list of changes to the release notes, under
   the heading `## What's changed`. For a stable release the list starts at
   the previous stable release, so it covers the betas in between too.

The install script `site/install.sh` (served as `irsync.org/install.sh`)
picks the newest release for its channel: stable by default, `--beta` for
betas and stable, `--alpha` for all. Other arguments go to the installer that
`release.yml` built. The installer puts the programs in `~/.local/bin`, and
has no updater. It writes a small receipt file,
`~/.config/irsync/irsync-receipt.json`; it is harmless. To stop it editing
the shell's `PATH` setup, set `IRSYNC_NO_MODIFY_PATH=1`.

## The release workflow is generated

`.github/workflows/release.yml` is made by `dist generate` (cargo dist
0.34.0). Never edit it by hand. The settings are in `dist-workspace.toml`,
and the package is described in `packaging/dist/dist.toml`. After you change
`dist-workspace.toml`, run:

```sh
dist generate
```

and commit the result.

`release.yml` only starts when the gate launches it. **Never launch it by
hand from the Actions tab.** That skips the tests.

## If a release fails

**The check failed.** Nothing ran. Usually the version in the two files does
not match the tag. A release tag can't be deleted or moved (see
[Protecting the release tags](#protecting-the-release-tags)), so fix the
version on `main` and tag the next version (for example `v1.2.4`).

**The test step failed.** Nothing was published. If it was a flaky test,
use "Re-run failed jobs" on the gate run: the tag and the commit stay the
same, so that is fine. If the failure is real, fix it on `main`, then tag a
new patch version (for example `v1.2.4`). Don't move the tag. If you move a
tag after pushing it, the gate refuses to release it, because the tests ran
on a different commit.

**The release step failed.** Look at the `release.yml` run it launched. The
link is in the gate's log. If the gate could not find the run, look in the
Actions tab under the release workflow: it may still be running.

- If the GitHub Release was not created, treat it like a failed test: fix,
  then tag a new patch version.
- If the GitHub Release was created, the release is public, but the gate
  stopped before the packages, Homebrew and the notes. This happens when
  the gate lost track of the `release.yml` run. Finish the release with the
  resume workflow (see below).

**The packages or Homebrew step failed.** Fix the cause (often a token in
the `release` environment), then re-run that job from the gate run, or use
the resume workflow. Both are safe to run again. If the packages repo
already has this version, the packages job builds nothing and changes
nothing there. If the tap already has the formula, the Homebrew job pushes
nothing.

**The notes step failed.** Re-run that job, or use the resume workflow. It
does nothing if the heading is already there.

Two traps:

- **Cancelling the gate run does not stop `release.yml`** once it has been
  launched. Cancel that run too, from the Actions tab.
- **Re-running the gate's `release` job launches a second `release.yml`.** If
  the GitHub Release already exists, the second run fails when it tries to
  create it. Re-run only the `packages`, `homebrew` or `notes` job, not
  `release`, or use the resume workflow.

### Finishing a release with the resume workflow

`.github/workflows/release-resume.yml` finishes a release that already
exists. It runs the same packages, Homebrew and notes steps as the gate. It
never builds or publishes a release. Start it **on the tag**, so the
`release` environment opens. In the browser, go to Actions, Release resume,
Run workflow, and pick the tag under "Use workflow from". Or run:

```sh
gh workflow run release-resume.yml --ref v1.2.3 -f tag=v1.2.3
```

It stops with an error if it runs on anything but that tag. It checks that
the tag is a release tag and that its GitHub Release exists and is
published. For a stable release it runs the packages and Homebrew steps
(after you approve `release`) and the notes. For a pre-release it only adds
the notes. It runs the scripts as they were at the tag. A fix to a release
script on `main` reaches the next release, not this one.

With `-f dry-run=true` it builds and signs the packages, for any release,
pre-releases too. It checks that the token may push to the packages repo,
but publishes nothing: no push, no Homebrew, no notes. The packages and the
updated repo are kept as artifacts of the run, for you to look at. Use it
on a pre-release to check the key and tokens before the first stable
release.

## One-time setup

Do this once, before the first release.

**Repositories.** Create two public repositories in the `irsync-dev`
organisation. Create both with a README, so they have a `main` branch:

- `homebrew-tap`, for the Homebrew formula.
- `packages`, for the apt and dnf repo.

**GitHub Pages.** In the `packages` repository, turn on Pages and serve the
default branch. Set the custom domain to `dist.irsync.org` and turn on
"Enforce HTTPS". You don't fill the repository by hand. Each stable release
copies `index.html`, `irsync.repo` and `CNAME` from `packaging/repo/` into
it, and exports the public key as `irsync.asc` (for dnf) and `irsync.gpg`
(for apt).

**DNS.** Add a `CNAME` record that points `dist.irsync.org` at
`irsync-dev.github.io`. If the DNS is on Cloudflare, set the record to
"DNS only" (grey cloud), so GitHub can issue the HTTPS certificate.

**Signing key.** Make a GPG key with the user ID
`irsync releases <releases@irsync.org>`, step by step in
[Making the signing key](#making-the-signing-key). Set up the
`releases@irsync.org` mailbox too, so mail to that address reaches someone.
The workflow uses the fingerprint of the key it imports as `GPG_KEY_ID`, so
there is nothing else to set.

**Tokens and secrets.** Make one fine-grained token per repository, and put
the secrets in the `release` environment, not the repository. Step by step
in [Supply chain security](supply-chain.md#maintainer-steps), steps 2 to 5:

| Secret (environment `release`) | What it holds |
| --- | --- |
| `PACKAGES_GPG_PRIVATE_KEY` | The signing **subkey** only: `private-subkey.asc` |
| `PACKAGES_GPG_PASSPHRASE` | The key's passphrase: `passphrase.txt`, with no newline at the end |
| `PACKAGES_REPO_TOKEN` | A token that can push to `irsync-dev/packages` and nothing else |
| `HOMEBREW_TAP_TOKEN` | A token that can push to `irsync-dev/homebrew-tap` and nothing else |

**Repository settings.** `tools/github_settings.py apply` sets the
rulesets (including [the release tags](#protecting-the-release-tags)), the
environments, secret scanning and immutable releases. See
[Checking the settings](supply-chain.md#checking-the-settings).

## The first release

Do the one-time setup first. Then:

1. **Merge to `main`.** The gate can only launch `release.yml` once it is on
   the default branch, so the merge comes first. The install steps in the
   README and the book go live with it. They say that Homebrew, apt and dnf
   start with v0.1.0, and that `--alpha` works before that.
2. **Put the install script on irsync.org.** The merge deploys the site,
   with `install.sh`, to staging. Check that
   `https://dev.irsync.org/install.sh` is there, then promote the site (see
   [Promoting to irsync.org](website.md#promoting-to-irsyncorg)).
3. **Rehearse with an alpha.** Set the version in both files to
   `0.1.0-alpha.1`, push to `main`, and push an annotated tag:

   ```sh
   git tag -a v0.1.0-alpha.1 -m "irsync 0.1.0-alpha.1"
   git push origin v0.1.0-alpha.1
   ```

   Check that:
   - the gate passes with the annotated tag;
   - the GitHub Release is marked as a pre-release and has the tarballs, the
     checksums and the installer;
   - its notes end with `## What's changed`;
   - the install script with `--alpha` installs it, and without `--alpha`
     says there is no stable release yet;
   - nothing went to Homebrew, apt or dnf.
4. **Dry-run the packages on the alpha.** An alpha skips Homebrew, apt and
   dnf, so the steps only stable releases run have not run yet. Run them
   without publishing:

   ```sh
   gh workflow run release-resume.yml --ref v0.1.0-alpha.1 -f tag=v0.1.0-alpha.1 -f dry-run=true
   ```

   This uses the GPG secrets, the packages repo token, nfpm and Ubuntu's
   rpm. Check that the run passes. Download its two artifacts and look at
   them: the `.deb` files list `/usr/bin/irsync` and `/usr/bin/fileset`
   (`dpkg-deb -c`), and the `.rpm` files are signed (`rpm -Kv`, after
   `rpm --import` of `irsync.asc` from the repo artifact).
5. **Check the Homebrew token.** Nothing has used `HOMEBREW_TAP_TOKEN` yet.
   Push a throwaway branch to the tap with it, then delete the branch. When
   git asks, give any user name and the token as the password. The
   `credential.helper=` setting stops git from using your own login instead:

   ```sh
   git clone https://github.com/irsync-dev/homebrew-tap
   cd homebrew-tap
   git -c credential.helper= push origin HEAD:refs/heads/token-check
   git -c credential.helper= push origin --delete token-check
   ```

6. **Cut v0.1.0.** Set the version in both files to `0.1.0` and follow
   [Cutting a release](#cutting-a-release). Then check the Homebrew formula,
   the apt and dnf repo at `dist.irsync.org`, and the signed `.rpm` files on
   the release.

## The packages repo grows

Every release adds a commit with new package files to `irsync-dev/packages`.
Pruning removes old files from the tree, but git keeps them in the history,
so the repository keeps growing. Pages serves only the current tree, so the
history is not needed. Once in a while, when no release is running, replace
it with a single commit:

```sh
git clone https://github.com/irsync-dev/packages
cd packages
git checkout --orphan fresh
git commit -m "Packages, history squashed"
git push --force origin fresh:main
```

If `main` is protected against force pushes, allow it for this push.

## Protecting the release tags

Pushing a `v*` tag starts a release, and the release workflow trusts the
commit the tag points at. The `Release tags` ruleset, set by
`tools/github_settings.py`, makes every `v*` tag permanent once pushed:

| Rule | What it stops |
| --- | --- |
| Restrict updates | Moving an existing tag to another commit |
| Restrict deletions | Deleting a tag |
| Block force pushes | Overwriting a tag with `git push --force` |

Nobody can bypass it, admins included. Creating tags is allowed, so anyone
with write access can start a release gate. A stable release still
publishes nothing beyond the GitHub Release until a maintainer approves the
`release` environment. Nothing in the release workflows creates, moves or
deletes tags.

A tag pushed by mistake stays. Release the next version instead. Check the
ruleset with:

```sh
tools/github_settings.py check
```

## Making the signing key

The key signs the apt and dnf repositories at `dist.irsync.org`. The steps
below make it, test it the way the workflow uses it, and leave you with the
files for the two GPG secrets. They take about ten minutes.

The key is RSA 4096, can only sign, has no subkeys and never expires:

- RSA works with every apt and rpm we care about, including older RHEL rpm
  versions that can't check newer key types.
- The workflow signs with the first secret key it finds, so one key with
  no subkeys is all it needs.
- apt users keep the public key in a file on their machine. If the key
  expired, `apt update` would fail for everyone until they downloaded it
  again. The revocation certificate covers a lost or leaked key instead.

The passphrase is long and random. Nobody types it; it lives only in the
secret and in your backup.

**1. A separate keyring.** Use a machine you trust, and keep the key out of
your everyday keyring. `GNUPGHOME` tells gpg which keyring to use, for this
terminal only, so run every later step in the same terminal:

```sh
umask 077
mkdir ~/irsync-signing-key
cd ~/irsync-signing-key
export GNUPGHOME="$PWD/gnupg"
mkdir -m 700 "$GNUPGHOME"
gpg --list-secret-keys    # prints nothing: the keyring is empty
```

**2. The passphrase.** 40 random characters, with no newline at the end.
The workflow uses the secret byte for byte, so a trailing newline would
become part of the passphrase and signing would fail:

```sh
gpg --gen-random --armor 1 30 | tr -d '\n' > passphrase.txt
wc -c passphrase.txt      # 40
```

**3. The key.** `rsa4096` is the key type, `sign` means it can only sign,
and `never` is the expiry date. `--pinentry-mode loopback` with
`--passphrase-file` takes the passphrase from the file instead of a
password prompt:

```sh
gpg --batch --pinentry-mode loopback --passphrase-file passphrase.txt \
    --quick-generate-key 'irsync releases <releases@irsync.org>' rsa4096 sign never
FPR=$(gpg --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')
echo "$FPR"
gpg --list-keys
```

`FPR` is the fingerprint, the key's 40-character ID. `gpg --list-keys`
shows one `rsa4096` key with usage `[SC]`, no expiry, and the user ID
`irsync releases <releases@irsync.org>`.

**4. Export the files.**

```sh
gpg --batch --pinentry-mode loopback --passphrase-file passphrase.txt \
    --armor --export-secret-keys "$FPR" > private-key.asc
gpg --armor --export "$FPR" > public-key.asc
cp "$GNUPGHOME/openpgp-revocs.d/$FPR.rev" revocation.rev
head -1 private-key.asc   # -----BEGIN PGP PRIVATE KEY BLOCK-----
```

| File | What it is | Where it goes |
| --- | --- | --- |
| `private-key.asc` | The private key, still protected by the passphrase | Your backup only. GitHub gets a signing subkey, `private-subkey.asc`, made in [Supply chain security](supply-chain.md#maintainer-steps) step 2 |
| `passphrase.txt` | The passphrase | `PACKAGES_GPG_PASSPHRASE`, and your backup |
| `revocation.rev` | Declares the key dead, even without the private key | Your backup only |
| `public-key.asc` | The public key | Your records; every stable release publishes it as `irsync.asc` and `irsync.gpg` |

gpg made the revocation certificate together with the key. Anyone who has
it can kill the key, so it never goes into GitHub.

**5. Test the files the way the workflow uses them.** First make the
signing subkey ([Supply chain security](supply-chain.md#maintainer-steps)
step 2). The workflow imports `private-subkey.asc` into an empty keyring,
takes the first fingerprint it finds (the primary key's, whose secret part
isn't there) and signs with the passphrase, which uses the subkey. Do the
same in a second empty keyring:

```sh
export GNUPGHOME="$PWD/ci-test"
mkdir -m 700 "$GNUPGHOME"
gpg --batch --import private-subkey.asc
TEST_FPR=$(gpg --batch --list-secret-keys --with-colons | awk -F: '$1 == "fpr" { print $10; exit }')
[ "$TEST_FPR" = "$FPR" ] && echo "fingerprint matches"
echo test > test.txt
gpg --batch --yes --pinentry-mode loopback --passphrase-file passphrase.txt \
    --local-user "$TEST_FPR" --clearsign -o test.txt.asc test.txt
gpg --verify test.txt.asc
```

You should see `fingerprint matches` and `Good signature from "irsync
releases <releases@irsync.org>"`. A warning that the key is not certified
with a trusted signature is normal. Then remove the test keyring:

```sh
gpgconf --kill gpg-agent
rm -rf "$GNUPGHOME" test.txt test.txt.asc
export GNUPGHOME="$PWD/gnupg"
```

**6. The secrets.** Reading from a file keeps the passphrase out of your
shell history:

```sh
gh secret set PACKAGES_GPG_PRIVATE_KEY --env release --repo irsync-dev/irsync < private-subkey.asc
gh secret set PACKAGES_GPG_PASSPHRASE  --env release --repo irsync-dev/irsync < passphrase.txt
gh secret list --env release --repo irsync-dev/irsync
```

In the browser instead (Settings, Environments, `release`), paste the whole
of `private-subkey.asc`, including the
`BEGIN` and `END` lines, and the 40 passphrase characters with nothing
after them. GitHub never shows a secret again, so if you are unsure you
pasted it right, set it again.

**7. Back up, then clean up.** Keep `private-key.asc`, `passphrase.txt` and
`revocation.rev` somewhere safe outside GitHub, such as a password manager
that takes attachments or an encrypted USB stick. Write the fingerprint
(`echo "$FPR"`) next to them: it is how anyone checks that a downloaded
`irsync.asc` is the real key. Without the backup, losing the secret means a
new key, and every user has to import it again.

Once you have checked that you can open the backup, delete the working
folder:

```sh
gpgconf --kill gpg-agent
cd ~
rm -rf ~/irsync-signing-key
unset GNUPGHOME
```

**8. Check it on a real run.** An alpha doesn't sign anything, so the
first real use of the key is the dry run in
[The first release](#the-first-release), step 4. A wrong passphrase
secret fails there. After the first stable release, check that the
published key is yours:

```sh
curl -fsSL https://dist.irsync.org/irsync.asc | gpg --show-keys
```

The fingerprint it shows must match the one you wrote down.

**If the key leaks,** mark it revoked where users fetch it, then rotate to
a new key (below). In a throwaway keyring, import the public key and the
revocation certificate, and export the result:

```sh
gpg --import public-key.asc revocation.rev
gpg --armor --export "$FPR" > irsync.asc
gpg --export "$FPR" > irsync.gpg
```

Commit these two files to the `packages` repository by hand. Anyone who
fetches the key from then on gets it marked revoked, and the next stable
release replaces both files with the new key. Users who already have the
old key keep trusting it until they import the new one, so tell them
(rotation step 4). **If the key is lost** with no backup, make a new key
and rotate.

## Rotating the signing key

1. Make the new key, with the same user ID, as in
   [Making the signing key](#making-the-signing-key).
2. Put the new key's signing subkey and passphrase into the
   `PACKAGES_GPG_PRIVATE_KEY` and `PACKAGES_GPG_PASSPHRASE` secrets of the
   `release` environment
   ([Supply chain security](supply-chain.md#maintainer-steps), steps 2 and 4).
   If only the subkey leaked, replace the subkey instead and keep the key.
3. Cut a stable release. It signs the metadata with the new key and
   publishes the new public key as `irsync.asc` and `irsync.gpg`.
4. Tell users to import the new key again. For apt, that means downloading
   `irsync.gpg` again to `/etc/apt/keyrings/`. For dnf, the `.repo` file
   fetches the key from `irsync.asc`; dnf asks to trust the new one.

Old packages stay signed with the old key until they are pruned from the
repo (after 3 newer versions). Until then, users who have not re-imported the
key may see signature errors on those files.

## The user-facing install steps

The README, the book's introduction and the fileset introduction list the
ways to install. The apt and dnf commands are in `packaging/repo/index.html`
(the page at `dist.irsync.org`). If you change them there, change them in
the docs too.
