#!/bin/sh
# Install irsync: find the newest release for a channel and run its installer.
#
#   curl -LsSf https://irsync.org/install.sh | sh
#   curl -LsSf https://irsync.org/install.sh | sh -s -- --beta
#
# Channels: stable by default; --beta also allows beta releases; --alpha allows
# alpha and beta. A channel flag must come first. All other arguments go to the
# release's own installer unchanged.
#
# For tests only: IRSYNC_INSTALL_RELEASES_FILE reads the releases JSON from a
# file instead of GitHub, and IRSYNC_INSTALL_DRY_RUN=1 prints what would be
# installed instead of downloading it.
set -eu

repo=irsync-dev/irsync
api="https://api.github.com/repos/$repo/releases?per_page=100"
page="https://github.com/$repo/releases"

say() { printf 'irsync: %s\n' "$*" >&2; }
die() { say "$*"; exit 1; }

channel=stable
case ${1-} in
  --beta) channel=beta; shift ;;
  --alpha) channel=alpha; shift ;;
esac

fetch() { # URL: print the body on stdout
  if command -v curl >/dev/null 2>&1; then
    curl -fsSL "$1"
  elif command -v wget >/dev/null 2>&1; then
    wget -qO- "$1"
  else
    die "need curl or wget to download releases"
  fi
}

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
# alpha=0 < beta=1 < stable=2. Tags of any other shape are ignored.
tag=$(printf '%s\n' "$releases" |
  grep -o '"tag_name"[[:space:]]*:[[:space:]]*"[^"]*"' |
  sed 's/.*:[[:space:]]*"\([^"]*\)"$/\1/' |
  grep -E '^v[0-9]+\.[0-9]+\.[0-9]+(-(alpha|beta)\.[0-9]+)?$' |
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

say "installing $tag"
# Download to a file first: a failed download must not run as an empty script.
tmp=$(mktemp -d "${TMPDIR:-/tmp}/irsync-install.XXXXXX")
trap 'rm -rf "$tmp"' EXIT
fetch "$url" >"$tmp/installer.sh" || die "could not download $url"
sh "$tmp/installer.sh" "$@"
