#!/bin/sh
# Deploy the site to Cloudflare Pages from a developer machine.
#
# Usage:
#   site/deploy.sh staging   build target/site, deploy it to irsync-dev
#                            (dev.irsync.org), keep a copy in target/site-staging/
#   site/deploy.sh promote   deploy target/site-staging/ unchanged to irsync-org
#                            (irsync.org)
#
# Credentials: CLOUDFLARE_API_TOKEN and CLOUDFLARE_ACCOUNT_ID, or `wrangler login`.
set -eu

# Kept in step with .github/workflows/site.yml.
WRANGLER_VERSION=4.20.0

root=$(cd "$(dirname "$0")/.." && pwd -P)
site=$root/target/site
staging=$root/target/site-staging
staging_url=https://dev.irsync.org

usage() {
  echo "usage: site/deploy.sh staging|promote" >&2
  exit 2
}

pages_deploy() { # DIR PROJECT
  npx --yes "wrangler@$WRANGLER_VERSION" pages deploy "$1" --project-name "$2" --branch main
}

[ $# -eq 1 ] || usage

case $1 in
  staging)
    build=$(git -C "$root" rev-parse HEAD)
    if [ -n "$(git -C "$root" status --porcelain)" ]; then
      echo "refusing to deploy $build-dirty: commit or stash the changes first" >&2
      exit 1
    fi
    "$root/site/build.sh" "$site"
    echo "$build" >"$site/BUILD"
    pages_deploy "$site" irsync-dev
    # Copy beside, then swap, so a failed copy never leaves a partial staging copy.
    rm -rf "$staging.new"
    cp -R "$site" "$staging.new"
    rm -rf "$staging"
    mv "$staging.new" "$staging"
    echo "staged $build; site/deploy.sh promote ships it to irsync.org"
    ;;
  promote)
    if [ ! -f "$staging/BUILD" ]; then
      echo "refusing to promote: $staging/BUILD is missing; run site/deploy.sh staging first" >&2
      exit 1
    fi
    # Promote only what dev.irsync.org serves: someone else may have deployed
    # staging since this machine's copy was made.
    build=$(cat "$staging/BUILD")
    served=$(curl -fsS "$staging_url/BUILD") || {
      echo "refusing to promote: cannot fetch $staging_url/BUILD" >&2
      exit 1
    }
    if [ "$served" != "$build" ]; then
      echo "refusing to promote: $staging holds $build, but $staging_url serves $served;" \
        "run site/deploy.sh staging again" >&2
      exit 1
    fi
    pages_deploy "$staging" irsync-org
    echo "promoted $build"
    ;;
  *) usage ;;
esac
