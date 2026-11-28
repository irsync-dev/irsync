# The release signing key

Every file on a GitHub Release is signed with
[minisign](https://jedisct1.github.io/minisign/), a small signing tool. The
key that makes those signatures never goes near GitHub. It lives on a USB
stick, and a maintainer signs each release on their own machine with
[`sign-release.sh`](../../../.github/scripts/sign-release.sh) (see [Releasing](releasing.md)). Because
the key is not on GitHub, someone who takes over the GitHub organisation or
a workflow still can't make a release that passes the check.

This is a different key from the GPG key that signs the apt and dnf
repositories ([Making the signing key](releasing.md#making-the-signing-key)).
That one lives in GitHub, in the `release` environment.

This page makes the key, once. It takes about fifteen minutes. At the end
you have:

- the **secret key**, a file protected by a password, on a USB stick and on
  a backup stick,
- the **password**, in your password manager,
- the **public key**, committed as [`release-keys.pub`](../../../packaging/signing/release-keys.pub).
  Everyone may see it: irsync, [`install.sh`](../../../site/install.sh) and users check signatures
  against it.

## What you need

**minisign 0.10 or newer.** Older versions make signatures in an old format
that irsync refuses.

```sh
sudo apt install minisign     # Debian, Ubuntu
sudo dnf install minisign     # Fedora
brew install minisign         # macOS
minisign -v                   # minisign 0.11 or similar
```

**Two USB sticks**, ideally encrypted (LUKS on Linux, an encrypted APFS
volume on macOS). One holds the key you sign with, the other is the backup.

**A password manager** for the key's password.

## Making the key

**1. Open the stick.** Plug in the first stick and note where it is
mounted. Run every later step in the same terminal:

```sh
KEYDIR=/media/$USER/irsync-key     # macOS: /Volumes/irsync-key
umask 077                          # new files readable by you only
ls "$KEYDIR"
```

**2. Choose the password.** Let your password manager make a long random
one, at least 20 characters, and save it there under "irsync release key".
You type it once per release, so it doesn't need to be easy to remember.

**3. Make the key pair.** minisign asks for the password twice:

```sh
minisign -G -p "$KEYDIR/irsync-release.pub" -s "$KEYDIR/irsync-release.key"
cat "$KEYDIR/irsync-release.pub"
```

The public key file has two lines. The first is a comment with the key ID,
16 hex digits that name the key. The second is the key itself:

```text
untrusted comment: minisign public key 1A2B3C4D5E6F7A8B
RWSLejxNPCsaG...
```

The secret key file is encrypted with the password. The file alone is
useless without the password, and the password alone is useless without
the file.

**4. Test the key the way a release uses it.** Sign a test file with a
trusted comment, as [`sign-release.sh`](../../../.github/scripts/sign-release.sh) does with `irsync vX.Y.Z`, then check
it:

```sh
cd "$(mktemp -d)"
echo test > test.txt
minisign -S -s "$KEYDIR/irsync-release.key" -t "irsync v0.0.0-test" -m test.txt
minisign -V -p "$KEYDIR/irsync-release.pub" -m test.txt
```

The last command prints `Signature and comment signature verified` and
`Trusted comment: irsync v0.0.0-test`.

Then check the signature format. The first two bytes of the signature must
be `ED`, which means minisign hashed the file before signing it. `Ed` means
the old format: upgrade minisign, delete the two key files and start again
at step 3.

```sh
sed -n 2p test.txt.minisig | base64 --decode | head -c 2; echo    # ED
```

Finally, check that a changed file fails:

```sh
echo changed >> test.txt
minisign -V -p "$KEYDIR/irsync-release.pub" -m test.txt     # Signature verification failed
```

**5. Make the backup.** Copy both files to the second stick and keep it
somewhere else, away from the first stick and from your computer:

```sh
cp "$KEYDIR/irsync-release.key" "$KEYDIR/irsync-release.pub" /media/$USER/irsync-key-backup/
```

The password stays in the password manager, never on either stick.

**6. Publish the public key.** In your irsync checkout, on a branch, copy
the public key into the repository and open a pull request:

```sh
cp "$KEYDIR/irsync-release.pub" packaging/signing/release-keys.pub
```

[`release-keys.pub`](../../../packaging/signing/release-keys.pub) is the one place the public key lives. irsync builds it
in, and the tests check that [`install.sh`](../../../site/install.sh) holds the same key. Also
update the key shown in [`README.md`](../../../README.md), in
[`SECURITY.md`](../../../SECURITY.md) and in
[Verify a download](../how-to/verify-a-download.md) in the same pull
request. The tests check README.md and SECURITY.md too.

**7. Put the stick away.** Unmount it and unplug it. It only needs to be
plugged in while you sign a release.

## Where the secret key must never go

- Into GitHub in any form: not a secret, not an environment, not the
  repository, not an issue.
- Into CI, or any machine you don't control.
- Into a folder that syncs to the cloud (Dropbox, iCloud Drive, Google
  Drive, OneDrive).
- Into the irsync checkout, where a careless `git add` could commit it.

## Signing a release

Plug in the stick, then follow [Releasing](releasing.md). The signing step
is:

```sh
.github/scripts/sign-release.sh --key "$KEYDIR/irsync-release.key" 1.2.3
```

It asks for the password once and signs every file on the draft release.
Check the draft on GitHub, then run the same command with `--publish`
before the version to publish it. Unplug the stick when it is done.

## A second key

So that releases don't depend on one person, another maintainer can make
their own key with steps 1 to 5, on their own sticks and with their own
password. Add their public key to [`release-keys.pub`](../../../packaging/signing/release-keys.pub) below the first one,
both lines of it. A release signed with either key passes. Also add the
key to [`README.md`](../../../README.md), [`SECURITY.md`](../../../SECURITY.md) and [`verify-a-download.md`](../../../book/src/how-to/verify-a-download.md).

A new key can sign from the first release that lists it. An irsync that is
already installed only checks downloads of its own version, which were
signed with the key in use back then, and [`install.sh`](../../../site/install.sh) comes from the
website with the current keys.

## If the key is lost or leaked

**Lost:** both sticks are gone, or the password is. Make a new key with the
steps above, add its public key to [`release-keys.pub`](../../../packaging/signing/release-keys.pub), remove the old one,
and release. Tell users the new key in the release notes, since anyone who
checks downloads by hand needs it.

**Leaked:** someone else may have the key file and the password. Do the
same as for a lost key, and in addition:

- Say so in the release notes and in a
  [security advisory](https://github.com/irsync-dev/irsync/security/advisories),
  with the old key ID.
- Tell users to upgrade: irsync binaries released before the change still
  trust the old key.
