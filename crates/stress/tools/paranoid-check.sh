#!/bin/bash
# Compare a finished irsync-stress root's destination tree with the source
# manifest on every field fileset records (--paranoid: also uid, gid, nlink,
# xattr), which the harness's own per-iteration check leaves out. For
# breadth.sh's CHECK hook, or by hand on any run root.
#
#   paranoid-check.sh <ROOT>
set -u
root=${1:?usage: paranoid-check.sh <ROOT>}
BIN=${BIN:-$(cd "$(dirname "$0")/../../.." && pwd)/target/release}
exec "$BIN/fileset" check -q --paranoid --manifest "$root/SRC/data/.fileset.sqlite" "$root/DST/data"
