# Keep a manifest current

A manifest describes the tree as it was when you recorded it. When you add,
edit or remove files on purpose, bring the manifest up to date:

```sh
fileset update ~/Photos
```

`update` hashes only files whose size, modification time or inode changed, or
that the manifest has no hash for. For every other file it keeps the stored
hash. It forgets files that are gone. The summary shows how much work it did:

```text
entries=8 hashed=1 reused=3 warnings=0
```

## Check before you update

Run `check` first on a tree you suspect, or have not looked at for a while:

```sh
fileset check ~/Photos
```

`update` keeps the stored hash of a file whose size, modification time and
inode are unchanged. So if a byte rotted in such a file, the stored hash
still holds the old content and the next `check` still shows the damage. But
an edited file is rehashed, and its new content becomes the truth. If rot and
an edit hit the same file, `update` hides the rot.

So read the `check` output first. A file you edited shows several fields
changed, such as `blake3`, `mtime` and `size`. Damage you did not cause often
shows `blake3` alone. When every difference is one you meant, run `update`.

## Record the current content as correct

`update --rehash` hashes every file again, even when size, modification time
and inode are unchanged:

```sh
fileset update --rehash ~/Photos
```

Use it only when you have decided the current content is right, for example
after you restored a file from a good copy. It takes as long as `init`. It
overwrites the stored hash of a damaged file, so the damage is no longer
visible.

## Keep a copy's manifest in step

After `update`, copy the tree to the backup again with your copy tool. The
updated manifest travels with it. Then `check` the copy. See
[Verify an rsync or rclone transfer](verify-other-tools.md).
