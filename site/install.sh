#!/bin/sh
# Install irsync: find the newest release for a channel and run its installer.
#
#   curl -LsSf https://irsync.org/install.sh | sh
#   curl -LsSf https://irsync.org/install.sh | sh -s -- --beta
#
# Channels: stable by default; --beta also allows beta releases; --alpha allows
# alpha and beta. --require-signature fails if the installer's signature
# cannot be checked (no irsync with verify-signature, and no minisign) instead
# of only warning. These three flags must come first, in any order. The first
# other argument and everything after it go to the release's own installer
# unchanged; --require-signature is never passed on.
#
# For tests only: IRSYNC_INSTALL_RELEASES_FILE reads the releases JSON from a
# file instead of GitHub, IRSYNC_INSTALL_DRY_RUN=1 prints what would be
# installed instead of downloading it, IRSYNC_INSTALL_DOWNLOAD_DIR reads
# irsync-installer.sh and irsync-installer.sh.minisig from that directory
# instead of downloading them, and IRSYNC_INSTALL_PUBLIC_KEYS (one key per
# line) replaces the built-in public keys.
set -eu

# Everything runs inside main, called on the last line, so a download cut short
# under `curl | sh` runs nothing.
main() {
  repo=irsync-dev/irsync
  api="https://api.github.com/repos/$repo/releases?per_page=100"
  page="https://github.com/$repo/releases"

  say() { printf 'irsync: %s\n' "$*" >&2; }
  die() { say "$*"; exit 1; }

  # The public keys of irsync's release signatures, one per line: the key
  # lines of packaging/signing/release-keys.pub. test_keys_match_release_keys_file
  # keeps the two in sync.
  keys="RWR6ngoFwyY7D9i4MAL2T4n1UkGwrCdSGA/SMowtANaffYWkTZdrAtAZ"
  if [ -n "${IRSYNC_INSTALL_PUBLIC_KEYS-}" ]; then
    keys=$IRSYNC_INSTALL_PUBLIC_KEYS
  fi
  # The first release with signatures. The ones before it are listed by name
  # below and never installed: they can't be checked, and an attacker who can
  # change releases could delete the newer ones to make the script pick one.
  first_signed=v0.1.0-beta.5

  channel=stable
  require_signature=
  while [ $# -gt 0 ]; do
    case $1 in
      --beta) channel=beta ;;
      --alpha) channel=alpha ;;
      --require-signature) require_signature=1 ;;
      *) break ;;
    esac
    shift
  done

  # HTTPS only, also after a redirect: a redirect to plain HTTP would let
  # anyone on the network path swap the download.
  fetch() { # URL: print the body on stdout
    if command -v curl >/dev/null 2>&1; then
      curl --proto '=https' --tlsv1.2 -fsSL "$1"
    elif command -v wget >/dev/null 2>&1; then
      wget --https-only -qO- "$1"
    else
      die "need curl or wget to download releases"
    fi
  }

  # BusyBox wget (Alpine's default) has no --https-only. Checked here, before
  # any download, so the message isn't lost inside a $(fetch ...).
  if ! command -v curl >/dev/null 2>&1 && command -v wget >/dev/null 2>&1 &&
     ! wget --help 2>&1 | grep -q -- --https-only; then
    die "this wget cannot refuse plain HTTP downloads; install curl and run again"
  fi

  unreachable() {
    die "could not reach GitHub to list releases. Check your network; GitHub also
  limits anonymous requests (the rate limit), so try again later. Releases are
  listed at $page"
  }

  if [ -n "${IRSYNC_INSTALL_RELEASES_FILE-}" ]; then
    releases=$(cat "$IRSYNC_INSTALL_RELEASES_FILE" 2>/dev/null) || unreachable
  else
    releases=$(fetch "$api") || unreachable
  fi

  # Each tag becomes a sortable record: X Y Z rank N tag, with rank
  # alpha=0 < beta=1 < stable=2. Tags of any other shape are ignored, and so
  # are numbers longer than 9 digits, which some awks can't hold.
  tag=$(printf '%s\n' "$releases" |
    grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' |
    sed 's/.*:[[:space:]]*"\([^"]*\)"$/\1/' |
    grep -E '^v[0-9]{1,9}\.[0-9]{1,9}\.[0-9]{1,9}(-(alpha|beta)\.[0-9]{1,9})?$' |
    awk -v channel="$channel" '
      {
        t = $0
        v = substr(t, 2)
        rank = 2; n = 0
        i = index(v, "-")
        if (i) {
          pre = substr(v, i + 1); v = substr(v, 1, i - 1)
          split(pre, p, ".")
          rank = (p[1] == "alpha") ? 0 : 1
          n = p[2] + 0
        }
        if (channel == "stable" && rank < 2) next
        if (channel == "beta" && rank < 1) next
        split(v, x, ".")
        if (!found || newer(x[1] + 0, x[2] + 0, x[3] + 0, rank, n)) {
          found = 1; b[1] = x[1] + 0; b[2] = x[2] + 0; b[3] = x[3] + 0
          b[4] = rank; b[5] = n; best = t
        }
      }
      function newer(a1, a2, a3, a4, a5) {
        if (a1 != b[1]) return a1 > b[1]
        if (a2 != b[2]) return a2 > b[2]
        if (a3 != b[3]) return a3 > b[3]
        if (a4 != b[4]) return a4 > b[4]
        return a5 > b[5]
      }
      END { if (found) print best }
    ')

  if [ -z "$tag" ]; then
    if [ "$channel" = stable ]; then
      die "no stable release yet. To install a pre-release, run again with --alpha. Releases are listed at $page"
    fi
    die "no release found. Releases are listed at $page"
  fi

  url="https://github.com/$repo/releases/download/$tag/irsync-installer.sh"

  if [ -n "${IRSYNC_INSTALL_DRY_RUN-}" ]; then
    printf 'would install %s %s' "$tag" "$url"
    for a in "$@"; do printf ' %s' "$a"; done
    printf '\n'
    exit 0
  fi

  # The releases from before signing, by name: no arithmetic to get wrong.
  case $tag in
    v0.1.0-alpha.*|v0.1.0-beta.[1-4])
      die "$tag is from before $first_signed, the first signed release, so its installer can't be checked; not installing it. Releases are listed at $page" ;;
  esac

  # Which tool checks the signature: irsync if it has verify-signature (older
  # ones do not), else minisign, else none.
  verifier=none
  if command -v irsync >/dev/null 2>&1 &&
       irsync verify-signature --help 2>&1 | grep -q minisign; then
    verifier=irsync
  elif command -v minisign >/dev/null 2>&1; then
    verifier=minisign
  elif [ -n "$require_signature" ]; then
    die "cannot check the installer's signature: install minisign, or drop --require-signature"
  else
    say "cannot check the installer's signature (install minisign to check it); relying on HTTPS and checksums"
  fi

  say "installing $tag"
  # Download to a file first: a failed download must not run as an empty script.
  tmp=$(mktemp -d "${TMPDIR:-/tmp}/irsync-install.XXXXXX")
  trap 'rm -rf "$tmp"' EXIT
  get() { # URL FILE: download, or copy from the test directory
    if [ -n "${IRSYNC_INSTALL_DOWNLOAD_DIR-}" ]; then
      cat "$IRSYNC_INSTALL_DOWNLOAD_DIR/${1##*/}" >"$2" 2>/dev/null
    else
      fetch "$1" >"$2"
    fi
  }
  get "$url" "$tmp/installer.sh" || die "could not download $url"

  bad_signature() {
    die "the signature of $url does not match: not running it. Please report this at https://github.com/irsync-dev/irsync/blob/main/SECURITY.md"
  }
  # irsync's own message, if any, stays on stderr.
  verify_irsync() {
    set --
    for k in $keys; do set -- "$@" --public-key "$k"; done
    irsync verify-signature "$@" --trusted-comment "irsync $tag" \
      --signature "$tmp/installer.sh.minisig" "$tmp/installer.sh" >/dev/null
  }
  verify_minisign() {
    for k in $keys; do
      # -H: only a prehashed signature passes. Not -q: it hides the comment.
      if out=$(minisign -V -H -P "$k" -x "$tmp/installer.sh.minisig" \
                 -m "$tmp/installer.sh" 2>/dev/null); then
        # Any failure is a bad signature (1), never irsync's usage error (2).
        printf '%s\n' "$out" | grep -qxF "Trusted comment: irsync $tag" || return 1
        return 0
      fi
    done
    return 1
  }
  case $verifier in
    irsync|minisign)
      get "$url.minisig" "$tmp/installer.sh.minisig" ||
        die "missing signature: could not download $url.minisig"
      "verify_$verifier" && status=0 || status=$?
      case $status in
        # The check required exactly this trusted comment, so it is the
        # signed one.
        0) say "good signature on irsync-installer.sh (checked with $verifier), trusted comment: irsync $tag" ;;
        # Exit 2 is irsync's usage error: it could not check, not a bad signature.
        2) die "irsync could not check the signature (see above); install minisign, or update irsync, and run again" ;;
        *) bad_signature ;;
      esac
      ;;
  esac

  sh "$tmp/installer.sh" "$@"
}

main "$@"
