#!/bin/bash
# Usage: sign-release.sh [--key PATH] [--no-publish] VERSION
#
# Signs the draft GitHub Release of VERSION with minisign, then publishes it.
# VERSION is written without the "v" (1.3.0, 1.3.0-beta.1). The release gate
# ends at the draft and says when to run this. Run it on your own computer:
# the signing key never goes to GitHub.
#
# Stops at the first failed check:
#  1. gh, minisign and sha256sum (or shasum) are there; the version is valid.
#  2. The release vVERSION exists and is a draft.
#  3. It holds exactly the files a release should have (signatures aside).
#  4. Every checksum in sha256.sum and in the .sha256 files matches.
#  5. Every file has a build attestation from release.yml.
#  6. You say yes to the list of files.
#  7. minisign signs them all in one go, with the trusted comment
#     "irsync vVERSION". --key picks the secret key; minisign's default is
#     ~/.minisign/minisign.key.
#  8. Every new signature verifies with a key in
#     packaging/signing/release-keys.pub, so a wrong key is caught here.
#  9. The signatures are uploaded (replacing old ones), and the release then
#     has one for every file.
# 10. Unless --no-publish: the tag must still not exist (a tag pushed by hand
#     while the draft waited would take over the release). The release is
#     published, must then be immutable, and release-resume.yml is started.
#
# IRSYNC_SIGN_KEYS_FILE replaces release-keys.pub; it is for the tests.
set -euo pipefail

usage() {
    echo "usage: $0 [--key PATH] [--no-publish] VERSION" >&2
    exit 2
}
die() {
    echo "sign-release.sh: $*" >&2
    exit 1
}

key=
publish=true
version=
while [ $# -gt 0 ]; do
    case $1 in
        --key)
            [ $# -ge 2 ] || usage
            key=$2
            shift 2
            ;;
        --no-publish)
            publish=false
            shift
            ;;
        -*) usage ;;
        *)
            [ -z "$version" ] || usage
            version=$1
            shift
            ;;
    esac
done
[ -n "$version" ] || usage

here=$(cd "$(dirname "$0")" && pwd)
root=$(cd "$here/../.." && pwd)
repo=irsync-dev/irsync
export GH_REPO=$repo
keys_file=${IRSYNC_SIGN_KEYS_FILE:-$root/packaging/signing/release-keys.pub}

# 1. Tools and version.
case $version in
    v*) die "Give the version without the v: ${version#v}" ;;
esac
tag=v$version
"$here/check-release-tag.sh" "$tag" >/dev/null
for tool in gh minisign; do
    command -v "$tool" >/dev/null || die "needs $tool, which is not on PATH."
done
if command -v sha256sum >/dev/null; then
    sha256_of() { sha256sum "$1" | cut -d ' ' -f 1; }
elif command -v shasum >/dev/null; then
    sha256_of() { shasum -a 256 "$1" | cut -d ' ' -f 1; }
else
    die "needs sha256sum or shasum, and neither is on PATH."
fi
[ -f "$keys_file" ] || die "$keys_file does not exist."

# 2. The draft.
info=$(gh release view "$tag" --json isDraft,assets,targetCommitish,databaseId \
    --jq '"\(.isDraft) \(.targetCommitish) \(.databaseId)", .assets[].name') ||
    die "Could not read the GitHub Release $tag (see the gh error above)."
{
    read -r is_draft commit id
    assets=$(cat)
} <<<"$info"
[ "$is_draft" = true ] || die "$tag is not a draft. Only a draft can be signed: a published release is immutable."
[[ $id =~ ^[0-9]+$ ]] || die "gh gave the release id '$id', which is not a number."

# 3. The files. The release must hold exactly these (plus signatures).
targets=$(sed -n 's/^targets *= *\[\(.*\)\]/\1/p' "$root/dist-workspace.toml" | tr -d '" ' | tr , ' ')
[ -n "$targets" ] || die "found no targets in dist-workspace.toml."
files=()
for t in $targets; do
    files+=("irsync-$t.tar.xz" "irsync-$t.tar.xz.sha256")
done
files+=(source.tar.gz source.tar.gz.sha256 irsync-installer.sh irsync.rb sha256.sum dist-manifest.json)

want=$(printf '%s\n' "${files[@]}" | LC_ALL=C sort)
have=$(printf '%s\n' "$assets" | grep -v '\.minisig$' | LC_ALL=C sort || true)
missing=$(LC_ALL=C comm -13 <(printf '%s\n' "$have") <(printf '%s\n' "$want") | grep . || true)
extra=$(LC_ALL=C comm -23 <(printf '%s\n' "$have") <(printf '%s\n' "$want") | grep . || true)
# An old signature is fine (it is replaced), but only for a release file.
while IFS= read -r name; do
    if ! printf '%s\n' "$want" | grep -qxF -- "${name%.minisig}"; then
        extra+=${extra:+$'\n'}$name
    fi
done < <(printf '%s\n' "$assets" | grep '\.minisig$' || true)
if [ -n "$missing" ] || [ -n "$extra" ]; then
    [ -z "$missing" ] || echo "$tag is missing these files:"$'\n'"$missing" >&2
    [ -z "$extra" ] || echo "$tag has files a release should not have:"$'\n'"$extra" >&2
    die "Not signing: the files on $tag are not what release.yml makes."
fi

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
dir=$tmp/files
mkdir "$dir"
echo "Downloading the files of $tag..."
gh release download "$tag" --dir "$dir" || die "Could not download the files of $tag."
rm -f "$dir"/*.minisig
paths=()
for f in "${files[@]}"; do
    [ -f "$dir/$f" ] || die "$f was not downloaded."
    paths+=("$dir/$f")
done

# 4. Checksums.
bad=()
for f in "${files[@]}"; do
    case $f in
        *.sha256)
            sum=$(awk '{ print $1; exit }' "$dir/$f")
            [ "$(sha256_of "$dir/${f%.sha256}")" = "$sum" ] || bad+=("${f%.sha256} (in $f)")
            ;;
    esac
done
[ -s "$dir/sha256.sum" ] || die "sha256.sum is empty."
while read -r sum name; do
    [ -n "$sum" ] || continue
    name=${name#\*}
    printf '%s\n' "$want" | grep -qxF -- "$name" ||
        die "sha256.sum names $name, which is not a release file."
    [ "$(sha256_of "$dir/$name")" = "$sum" ] || bad+=("$name (in sha256.sum)")
done <"$dir/sha256.sum"
if [ ${#bad[@]} -gt 0 ]; then
    printf '  %s\n' "${bad[@]}" >&2
    die "Not signing: the checksum of these files does not match."
fi

# 5. Build attestations.
echo "Checking the build attestations..."
for f in "${files[@]}"; do
    if ! gh attestation verify "$dir/$f" --repo "$repo" \
            --signer-workflow "$repo/.github/workflows/release.yml" >"$tmp/attestation.log" 2>&1; then
        cat "$tmp/attestation.log" >&2
        die "Not signing: the build attestation of $f does not verify."
    fi
done

# 6. Ask.
echo
echo "Release: $tag (draft)"
echo "Commit:  $commit"
echo "Files:"
printf '  %s\n' "${files[@]}"
printf 'Sign these files? [y/N] '
answer=
read -r answer || true
case $answer in
    y | Y | yes | Yes | YES) ;;
    *) die "Nothing signed." ;;
esac

# 7. Sign, all files in one go (one password prompt).
sign_key=()
if [ -n "$key" ]; then
    sign_key=(-s "$key")
fi
# ${sign_key[@]+...}: bash 3.2 on macOS calls an empty array unset.
minisign -S ${sign_key[@]+"${sign_key[@]}"} -t "irsync $tag" -m "${paths[@]}" ||
    die "minisign failed; nothing was uploaded."

# 8. Check every signature against the public keys, one key at a time.
mkdir "$tmp/keys"
awk -v d="$tmp/keys" '/^untrusted comment:/ { n++ } NF && n { print > (d "/" n ".pub") }' "$keys_file"
keys=("$tmp"/keys/*.pub)
[ -f "${keys[0]}" ] || die "$keys_file holds no key."
for f in "${files[@]}"; do
    ok=false
    for k in "${keys[@]}"; do
        if comment=$(minisign -V -Q -p "$k" -m "$dir/$f" 2>/dev/null); then
            ok=true
            break
        fi
    done
    $ok || die "The signature of $f does not verify with any key in $keys_file. Did you sign with the right key? Nothing was uploaded."
    [ "$comment" = "irsync $tag" ] ||
        die "The signature of $f has the trusted comment '$comment', not 'irsync $tag'. Nothing was uploaded."
    # Line 2 is the signature. "RU" is base64 for "ED", a prehashed
    # signature; the legacy kind ("Ed") is refused by irsync.
    [ "$(sed -n 2p "$dir/$f.minisig" | cut -c 1-2)" = RU ] ||
        die "The signature of $f is in minisign's legacy format. Use minisign 0.10 or newer, without -l."
done

# 9. Upload, then check every file has its signature.
sigs=()
for f in "${files[@]}"; do
    sigs+=("$dir/$f.minisig")
done
gh release upload "$tag" --clobber "${sigs[@]}" ||
    die "The upload failed. The release is still a draft; run this script again."
assets=$(gh release view "$tag" --json assets --jq '.assets[].name') ||
    die "Could not read the files of $tag after the upload."
missing=()
for f in "${files[@]}"; do
    printf '%s\n' "$assets" | grep -qxF -- "$f.minisig" || missing+=("$f.minisig")
done
if [ ${#missing[@]} -gt 0 ]; then
    printf '  %s\n' "${missing[@]}" >&2
    die "These signatures are not on $tag after the upload. The release is still a draft; run this script again."
fi
echo "Signed $tag and uploaded the signatures."

if ! $publish; then
    echo "The release is still a draft (--no-publish). To publish it, run this script again without --no-publish."
    exit 0
fi

# 10. Publish. The tag must not exist yet: publishing the draft creates it.
tags=$(gh api --paginate "repos/$repo/git/matching-refs/tags/$tag" --jq '.[].ref | ltrimstr("refs/tags/")') ||
    die "Could not look up the tag $tag. The release is still a draft."
if printf '%s\n' "$tags" | grep -qxF -- "$tag"; then
    die "The tag $tag exists, but its release is still a draft: someone pushed the tag by hand. Not publishing. Delete the draft and release the next version."
fi
# Not `gh release edit --draft=false`: that marks every release Latest, even
# a patch on an older line. make_latest=legacy lets GitHub decide by date and
# version, as `gh release create` does.
gh api -X PATCH "repos/$repo/releases/$id" -F draft=false -f make_latest=legacy >/dev/null ||
    die "Publishing $tag failed. It is signed; run this script again."
state=$(gh release view "$tag" --json isDraft,isImmutable --jq '"\(.isDraft) \(.isImmutable)"') ||
    die "Could not read $tag after publishing it. Check it on GitHub; release-resume.yml has not been started."
[ "$state" = "false true" ] ||
    die "$tag should now be published and immutable, but gh says isDraft isImmutable = $state. Check the release and the repository's immutable releases setting; release-resume.yml has not been started."
echo "Published $tag."

gh workflow run release-resume.yml --ref "$tag" -f tag="$tag" ||
    die "Could not start release-resume.yml. Start it with: gh workflow run release-resume.yml --ref $tag -f tag=$tag"
echo "Started release-resume.yml for $tag: packages, Homebrew and release notes."
