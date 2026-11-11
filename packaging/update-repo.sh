#!/bin/bash
# Usage: update-repo.sh <repo-checkout> <packages-dir> <keep>
# Adds the .deb and .rpm files in <packages-dir> to the repo, keeps the newest
# <keep> versions per architecture and format, and regenerates and signs the
# metadata. Safe to run again with the same packages.
#
# Environment: GPG_KEY_ID (required) selects the signing key; GNUPGHOME is
# honoured; GPG_PASSPHRASE is optional (otherwise gpg-agent decides).
set -euo pipefail

[ $# -eq 3 ] || { echo "usage: $0 <repo-checkout> <packages-dir> <keep>" >&2; exit 2; }
repo=$1
pkgs=$2
keep=$3
: "${GPG_KEY_ID:?GPG_KEY_ID must be set}"
case $keep in ''|*[!0-9]*|0) echo "keep must be a positive number" >&2; exit 2;; esac

here=$(cd "$(dirname "$0")" && pwd)
mkdir -p "$repo"
repo=$(cd "$repo" && pwd)
pkgs=$(cd "$pkgs" && pwd)

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# gpg options. The passphrase goes through a private file, never the command line.
gpg_base=(--batch --yes)
if [ -n "${GPG_PASSPHRASE:-}" ]; then
    umask 077
    printf '%s' "$GPG_PASSPHRASE" > "$tmp/pass"
    gpg_base+=(--pinentry-mode loopback --passphrase-file "$tmp/pass")
fi
gpg_opts=("${gpg_base[@]}" --local-user "$GPG_KEY_ID")

# rpmsign calls %__gpg; point it at a wrapper that adds our options.
real_gpg=$(command -v gpg)
{
    printf '#!/bin/sh\nexec %q' "$real_gpg"
    printf ' %q' "${gpg_base[@]}"
    printf ' "$@"\n'
} > "$tmp/gpg-wrapper"
chmod 700 "$tmp/gpg-wrapper"

# Keep the newest $keep versions of the files given on stdin as "version<TAB>path".
prune() {
    sort -t "$(printf '\t')" -k1,1V | head -n "-$keep" | cut -f2 | while IFS= read -r f; do
        [ -n "$f" ] && rm -f -- "$f"
    done || true
}

# ---- add packages ----
shopt -s nullglob
debs=("$pkgs"/irsync_*_{amd64,arm64}.deb)
rpms=("$pkgs"/irsync-*-1.{x86_64,aarch64}.rpm)
if [ $(( ${#debs[@]} + ${#rpms[@]} )) -eq 0 ]; then
    echo "no irsync .deb or .rpm packages found in $pkgs" >&2
    exit 1
fi

# Sign the rpms in a temp dir first, so an unsigned rpm never enters the repo
# and pruning cannot remove a file we still have to sign.
rpm_defs=(--define "_gpg_name $GPG_KEY_ID" --define "__gpg $tmp/gpg-wrapper")
[ -n "${GNUPGHOME:-}" ] && rpm_defs+=(--define "_gpg_path $GNUPGHOME")
mkdir "$tmp/rpms"
for f in "${rpms[@]}"; do cp -f "$f" "$tmp/rpms/"; done
signed=("$tmp"/rpms/*.rpm)
if [ ${#signed[@]} -gt 0 ]; then
    rpmsign "${rpm_defs[@]}" --addsign "${signed[@]}"
fi

pool="$repo/apt/pool/main/i/irsync"
mkdir -p "$pool"
for f in "${debs[@]}"; do cp -f "$f" "$pool/"; done
for arch in x86_64 aarch64; do
    mkdir -p "$repo/rpm/$arch"
    for f in "$tmp"/rpms/irsync-*-1."$arch".rpm; do cp -f "$f" "$repo/rpm/$arch/"; done
done

# ---- prune ----
for arch in amd64 arm64; do
    for f in "$pool"/irsync_*_"$arch".deb; do
        v=${f##*/irsync_}; v=${v%_"$arch".deb}
        printf '%s\t%s\n' "$v" "$f"
    done | prune
done
for arch in x86_64 aarch64; do
    for f in "$repo/rpm/$arch"/irsync-*-1."$arch".rpm; do
        v=${f##*/irsync-}; v=${v%-1."$arch".rpm}
        printf '%s\t%s\n' "$v" "$f"
    done | prune
done

# ---- apt metadata ----
apt="$repo/apt"
dist="$apt/dists/stable"
rm -f "$dist/Release" "$dist/InRelease" "$dist/Release.gpg"
for arch in amd64 arm64; do
    d="$dist/main/binary-$arch"
    mkdir -p "$d"
    (cd "$apt" && apt-ftparchive --arch "$arch" packages pool/main/i/irsync) > "$d/Packages"
    gzip -9 -n -c "$d/Packages" > "$d/Packages.gz"
done
apt-ftparchive \
    -o APT::FTPArchive::Release::Origin=irsync \
    -o APT::FTPArchive::Release::Label=irsync \
    -o APT::FTPArchive::Release::Suite=stable \
    -o APT::FTPArchive::Release::Codename=stable \
    -o APT::FTPArchive::Release::Architectures="amd64 arm64" \
    -o APT::FTPArchive::Release::Components=main \
    release "$dist" > "$tmp/Release"
mv "$tmp/Release" "$dist/Release"
gpg "${gpg_opts[@]}" --clearsign -o "$dist/InRelease" "$dist/Release"
gpg "${gpg_opts[@]}" --armor --detach-sign -o "$dist/Release.gpg" "$dist/Release"

# ---- rpm repo metadata (packages are already signed) ----
for arch in x86_64 aarch64; do
    d="$repo/rpm/$arch"
    rm -rf "$d/repodata"
    createrepo_c --quiet "$d"
    gpg "${gpg_opts[@]}" --armor --detach-sign -o "$d/repodata/repomd.xml.asc" "$d/repodata/repomd.xml"
done

# ---- keys and static files ----
gpg --batch --armor --export "$GPG_KEY_ID" > "$repo/irsync.asc"
gpg --batch --export "$GPG_KEY_ID" > "$repo/irsync.gpg"
cp -f "$here/repo/index.html" "$here/repo/irsync.repo" "$here/repo/CNAME" "$repo/"
