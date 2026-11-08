#!/bin/sh
# Serve the site locally, rebuilding it and reloading the browser on change.
# Usage: site/serve.sh [PORT]   (default: 9999)
#
# Builds target/site once, then runs two tools through npx: chokidar-cli
# reruns site/build.sh when a file under book/ or site/ changes, and
# browser-sync serves target/site and reloads open pages when it changes.
# Stop both with Ctrl-C.
set -eu

CHOKIDAR_VERSION=3.0.0
BROWSER_SYNC_VERSION=3.0.3

root=$(cd "$(dirname "$0")/.." && pwd -P)
port=${1:-9999}
cd "$root"

if ! command -v mdbook >/dev/null 2>&1; then
  echo "mdbook not found; install it with: cargo install mdbook" >&2
  exit 1
fi

site/build.sh

# The build writes book/theme/tokens.css; watching it would rebuild forever.
npx -y "chokidar-cli@$CHOKIDAR_VERSION" 'book/**' 'site/**' \
  -i '{book/theme/tokens.css,**/__pycache__/**}' \
  -c site/build.sh &

npx -y "browser-sync@$BROWSER_SYNC_VERSION" start --server target/site \
  --port "$port" --files target/site --no-notify --no-ui --no-open &

# npx runs each tool as a child of its own, so killing the npx processes
# would leave the tools running; signal the whole process group instead.
cleanup() {
  trap - EXIT INT TERM
  kill 0 2>/dev/null
}
trap cleanup EXIT
trap 'exit 130' INT TERM
echo "serving http://localhost:$port/ (Ctrl-C to stop)"
wait
