#!/bin/sh
# Draw the pictures irsync shows on GitHub from their sources in site/github/:
# the org avatar (avatar.svg, 512x512) and the repo's social preview
# (social-preview.html, 1280x640). GitHub takes only PNG or JPG, and both
# are uploaded by hand: the avatar under the org's Settings, Profile; the
# preview under the repo's Settings, General, Social preview.
# Needs Google Chrome or Chromium, and site/node_modules for the fonts.
# Usage: site/tools/github-images.sh
set -eu

site=$(cd "$(dirname "$0")/.." && pwd -P)
dir=$site/github

chrome=${CHROME:-}
if [ -z "$chrome" ]; then
  for c in "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome" \
    google-chrome chromium chromium-browser; do
    if command -v "$c" >/dev/null 2>&1; then
      chrome=$c
      break
    fi
  done
fi
if [ -z "$chrome" ]; then
  echo "Google Chrome or Chromium not found; set CHROME to its path" >&2
  exit 1
fi
if [ ! -d "$site/node_modules/@fontsource" ]; then
  (cd "$site" && npm ci --ignore-scripts --no-audit --no-fund)
fi

shot() { # SOURCE OUT WIDTH HEIGHT
  "$chrome" --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=1 \
    --allow-file-access-from-files --virtual-time-budget=2000 \
    --window-size="$3,$4" --screenshot="$2" "file://$1" 2>/dev/null
  echo "wrote $2"
}
shot "$dir/avatar.svg" "$dir/avatar.png" 512 512
shot "$dir/social-preview.html" "$dir/social-preview.png" 1280 640
