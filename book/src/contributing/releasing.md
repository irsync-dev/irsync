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

Any other tag that starts with `v` runs the tests and then stops with an
error. Nothing is published.

## Cutting a release

1. Set the new version in two places. They must match:
   - `[workspace.package] version` in `Cargo.toml`
   - `version` in `packaging/dist/dist.toml`

   A test fails if they differ.
2. Commit, and push to `main`.
3. Wait for CI on `main` to go green.
4. Tag the commit and push the tag:

   ```sh
   git tag v1.2.3
   git push origin v1.2.3
   ```

For a pre-release, use a tag such as `v1.2.3-beta.1` or `v1.2.3-alpha.1`. The
version in the two files is the same as for the tag, without the `v`:
`1.2.3-beta.1`.

## What the workflow does

Pushing the tag starts `.github/workflows/release-gate.yml`. It is the only
way a release starts. It does this, in order:

1. **Test.** It runs `ci.yml` on the tagged commit, with the musl release
   targets added. If anything fails, it stops. Nothing is published.
2. **Release.** It checks the tag name, and checks that the tag still points
   at the commit that was tested. Then it launches `release.yml` for the tag
   and waits for it. `release.yml` builds `irsync` and `fileset` for Linux
   x86_64, Linux arm64 and macOS on Apple Silicon. It publishes the GitHub
   Release with the tarballs, checksums and the install script. For stable
   tags it also pushes the Homebrew formula to `irsync-dev/homebrew-tap`.
3. **Packages** (stable tags only). `.github/workflows/packages.yml` builds
   the `.deb` and `.rpm` files from the released Linux tarballs and attaches
   them to the release. Then it adds them to `irsync-dev/packages`, signs the
   apt and dnf metadata and pushes. It keeps the newest 3 versions. GitHub
   Pages serves the result at `dist.irsync.org`.
4. **Notes.** It adds GitHub's list of changes to the release notes, under
   the heading `## What's changed`.

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

**The test step or the tag check failed.** Nothing was published. Fix the
problem on `main`, then tag a new patch version (for example `v1.2.4`).
Don't move the tag. If you move a tag after pushing it, the gate refuses to
release it, because the tests ran on a different commit.

**The release step failed.** Look at the `release.yml` run it launched. The
link is in the gate's log. If the GitHub Release was not created, treat it
like a failed test: fix, then tag a new patch version.

**The packages step failed.** Re-run that job from the gate run. It is safe
to run again: it replaces the files on the release and changes nothing in
the packages repo if it is already up to date.

**The notes step failed.** Re-run that job. It does nothing if the heading is
already there.

Two traps:

- **Cancelling the gate run does not stop `release.yml`** once it has been
  launched. Cancel that run too, from the Actions tab.
- **Re-running the gate's `release` job launches a second `release.yml`.** If
  the GitHub Release already exists, the second run fails when it tries to
  create it. Re-run only the `packages` or `notes` job, not `release`.

## One-time setup

Do this once, before the first release.

**Repositories.** Create two public repositories in the `irsync-dev`
organisation:

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
