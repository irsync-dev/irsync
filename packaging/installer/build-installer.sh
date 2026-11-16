#!/bin/sh
# Write a release's irsync-installer.sh: fill irsync-installer.sh.in with the
# version, the download URL and the three tarballs' SHA-256 checksums.
#
#   packaging/installer/build-installer.sh DISTRIB_DIR OUT_FILE
#
# DISTRIB_DIR holds dist's irsync-<target>.tar.xz.sha256 files. The version
# comes from packaging/dist/dist.toml.
#
# dist runs this as an extra-artifacts build. It ignores our exit status and
# fails only when OUT_FILE is missing, so any error leaves no OUT_FILE: a stale
# one is removed first, and the new one is renamed into place as the last step.
set -eu

die() { printf 'build-installer: %s\n' "$*" >&2; exit 1; }

[ $# -eq 2 ] || { echo "usage: build-installer.sh DISTRIB_DIR OUT_FILE" >&2; exit 2; }
distrib=$1
out=$2
rm -f "$out"

# CDPATH= keeps cd from searching CDPATH and printing where it went.
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
template=$here/irsync-installer.sh.in
dist_toml=$here/../dist/dist.toml

# The version line of the [package] table.
version=$(awk '
  /^\[/ { in_package = ($0 == "[package]") }
  in_package && /^version = "/ { sub(/^version = "/, ""); sub(/"$/, ""); print; exit }
' "$dist_toml")
case $version in
  '' | *[!0-9A-Za-z.+-]*) die "no usable version in $dist_toml: '$version'" ;;
esac
base_url=https://github.com/irsync-dev/irsync/releases/download/v$version

# The first field of DISTRIB_DIR/irsync-TARGET.tar.xz.sha256, which must be
# 64 lowercase hex digits.
checksum() {
  file=$distrib/irsync-$1.tar.xz.sha256
  [ -f "$file" ] || die "missing $file"
  sum=$(awk 'NR == 1 { print $1 }' "$file")
  case $sum in
    '' | *[!0-9a-f]*) die "no SHA-256 in $file" ;;
  esac
  [ ${#sum} -eq 64 ] || die "no SHA-256 in $file"
  printf '%s\n' "$sum"
}

sha_darwin=$(checksum aarch64-apple-darwin)
sha_linux_arm=$(checksum aarch64-unknown-linux-musl)
sha_linux_x86=$(checksum x86_64-unknown-linux-musl)

tmp=$(mktemp "$out.XXXXXX")
trap 'rm -f "$tmp"' EXIT

# The values hold no '|', '&' or '\', so they are safe in sed replacements.
sed -e "s|@VERSION@|$version|g" \
    -e "s|@BASE_URL@|$base_url|g" \
    -e "s|@SHA256_AARCH64_APPLE_DARWIN@|$sha_darwin|g" \
    -e "s|@SHA256_AARCH64_UNKNOWN_LINUX_MUSL@|$sha_linux_arm|g" \
    -e "s|@SHA256_X86_64_UNKNOWN_LINUX_MUSL@|$sha_linux_x86|g" \
    "$template" >"$tmp"

if grep -E '@[A-Z0-9_]+@' "$tmp" >&2; then
  die "placeholders left unfilled in $template"
fi
sh -n "$tmp" || die "the generated installer is not valid sh"
chmod 0755 "$tmp"
mv -f "$tmp" "$out"
