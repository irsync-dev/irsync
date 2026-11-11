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

1. Set the new version in two places. They must match:
   - `[workspace.package] version` in `Cargo.toml`
   - `version` in `packaging/dist/dist.toml`

   A test fails if they differ, and the release stops at once if they don't
   match the tag.
2. Commit, and push to `main`.
3. Wait for CI on `main` to go green.
4. Tag the commit and push the tag. Use an annotated tag (`-a`):

   ```sh
   git tag -a v1.2.3 -m "irsync 1.2.3"
   git push origin v1.2.3
   ```

For a pre-release, use a tag such as `v1.2.3-beta.1` or `v1.2.3-alpha.1`. The
version in the two files is the same as for the tag, without the `v`:
`1.2.3-beta.1`.

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
   Release with the tarballs, checksums and the install script. For stable
   tags it also pushes the Homebrew formula to `irsync-dev/homebrew-tap`.
4. **Packages** (stable tags only). `.github/workflows/packages.yml` builds
   the `.deb` and `.rpm` files from the released Linux tarballs. It signs the
   `.rpm` files, adds everything to `irsync-dev/packages`, signs the apt and
   dnf metadata and pushes. It keeps the newest 3 versions. GitHub Pages
   serves the result at `dist.irsync.org`. Then it attaches the same files,
   signed `.rpm` files included, to the release.
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
not match the tag. Delete the tag (`git push origin --delete v1.2.3` and
`git tag -d v1.2.3`), fix the version on `main`, and tag again.

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
  stopped before the packages and the notes. This happens when the Homebrew
  push failed, or when the gate lost track of the `release.yml` run. If the
  Homebrew push failed, fix the cause (often the `HOMEBREW_TAP_TOKEN`) and
  use "Re-run failed jobs" on that `release.yml` run. That runs only the
  Homebrew step and the jobs after it, not the release again. Then finish
  the release with the resume workflow (see below).

**The packages step failed.** Re-run that job from the gate run, or use the
resume workflow. It is safe to run again. If the packages repo already has
this version, it builds nothing and changes nothing there; it only attaches
any files the release is still missing.

**The notes step failed.** Re-run that job, or use the resume workflow. It
does nothing if the heading is already there.

Two traps:

- **Cancelling the gate run does not stop `release.yml`** once it has been
  launched. Cancel that run too, from the Actions tab.
- **Re-running the gate's `release` job launches a second `release.yml`.** If
  the GitHub Release already exists, the second run fails when it tries to
  create it. Re-run only the `packages` or `notes` job, not `release`, or use
  the resume workflow.

### Finishing a release with the resume workflow

`.github/workflows/release-resume.yml` finishes a release that already
exists. It runs the same packages and notes steps as the gate. It never
builds or publishes a release. Start it on `main`, in the browser (Actions,
Release resume, Run workflow) or with:

```sh
gh workflow run release-resume.yml --ref main -f tag=v1.2.3
```

It checks that the tag is a release tag and that its GitHub Release exists
and is published. For a stable release it runs the packages step and the
notes. For a pre-release it only adds the notes.

With `-f dry-run=true` it builds and signs the packages, for any release,
pre-releases too. It checks that the token may push to the packages repo,
but publishes nothing: no files on the release, no push, no notes. The
packages and the updated repo are kept as artifacts of the run, for you to
look at.

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
`irsync releases <releases@irsync.org>`. Set up the `releases@irsync.org`
mailbox too, so mail to that address reaches someone. The workflow uses the
fingerprint of the key it imports as `GPG_KEY_ID`, so there is nothing else to
set.

**Token.** Make a token that can push to `homebrew-tap` and `packages` and
nothing else. A fine-grained personal access token with "Contents: read and
write" on those two repositories works.

**Secrets.** In the `irsync-dev/irsync` repository, under Settings, Secrets
and variables, Actions, add four secrets:

| Secret | What it holds |
| --- | --- |
| `PACKAGES_GPG_PRIVATE_KEY` | The private signing key, exported with `gpg --armor --export-secret-keys` |
| `PACKAGES_GPG_PASSPHRASE` | The key's passphrase |
| `PACKAGES_REPO_TOKEN` | The token, for pushing to `irsync-dev/packages` |
| `HOMEBREW_TAP_TOKEN` | The token, for pushing to `irsync-dev/homebrew-tap` (cargo dist uses this name) |

You can use the same token for both token secrets.

**Tag protection (recommended).** Add a rule that protects tags matching `v*`,
so only maintainers can push or move release tags.

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
   gh workflow run release-resume.yml --ref main -f tag=v0.1.0-alpha.1 -f dry-run=true
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

## Rotating the signing key

1. Make the new key, with the same user ID.
2. Put the new private key and passphrase into the `PACKAGES_GPG_PRIVATE_KEY`
   and `PACKAGES_GPG_PASSPHRASE` secrets.
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
