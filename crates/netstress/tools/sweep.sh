#!/bin/bash
# Network-fault sweep: many seeds, every profile, both directions, several at
# a time. Roots of passing runs are deleted (a pass is reproducible from its
# seed); roots of failing runs are kept for diagnosis.
#
#   crates/netstress/tools/sweep.sh [EXTRA irsync-netstress ARGS...]
#
# Needs the fault build (it refuses to start without it):
#   cargo build --release --workspace --features netsim --target-dir target/netsim
#
# Environment (defaults in brackets):
#   BIN      directory holding irsync-netstress, irsync and the tools
#            [target/netsim/release]
#   OUT      where roots, per-run logs and summary.txt go
#            [$HOME/work/irsync-netstress-<date>]
#   SEEDS    seeds to run, a seq(1) expression                  [1 10]
#   JOBS     runs at a time                                     [4]
#   PROFILES profiles to run, a space-separated list    [4g-vehicle dsl-video]
#
# A run takes up to about 10 minutes. The exit is non-zero if any run's exit
# was not 0 (1 failed iteration, 2 rig error, 3 faults did not bite).
# Extra arguments go to irsync-netstress before the root, e.g.
#   sweep.sh --iterations 3
set -u
BIN=${BIN:-$(cd "$(dirname "$0")/../../.." && pwd)/target/netsim/release}
OUT=${OUT:-$HOME/work/irsync-netstress-$(date +%Y-%m-%d)}
SEEDS=${SEEDS:-1 10}
JOBS=${JOBS:-4}
PROFILES=${PROFILES:-4g-vehicle dsl-video}
for t in irsync-netstress irsync filegen fileset; do
  [ -x "$BIN/$t" ] || { echo "missing $BIN/$t (see the build command in this script's header)" >&2; exit 2; }
done
case $("$BIN/irsync" --version) in
  *" [netsim]") ;;
  *) echo "$BIN/irsync is not a fault build (--version does not end with [netsim])" >&2; exit 2 ;;
esac
mkdir -p "$OUT"
export BIN OUT
# Not an array: macOS's bash 3.2 calls an empty one unbound under `set -u`.
export EXTRA_ARGS="$*"

run() {
  profile=$1; dir=$2; seed=$3; root=$OUT/$profile-$dir-$seed
  # shellcheck disable=SC2086  # EXTRA_ARGS is a word list by design
  "$BIN/irsync-netstress" --profile "$profile" --direction "$dir" --seed "$seed" \
    --keep-going $EXTRA_ARGS "$root" > "$OUT/$profile-$dir-$seed.out" 2>&1
  rc=$?
  echo "$profile $dir $seed exit=$rc $(tail -1 "$OUT/$profile-$dir-$seed.out")" >> "$OUT/summary.txt"
  [ $rc -eq 0 ] && rm -rf "$root"
}
export -f run

: > "$OUT/summary.txt"
# shellcheck disable=SC2086  # SEEDS is "first last" for seq, PROFILES a list
for seed in $(seq $SEEDS); do for profile in $PROFILES; do for dir in push pull; do
  echo "$profile $dir $seed"
done; done; done | xargs -P "$JOBS" -n 3 "$BASH" -c 'run "$0" "$1" "$2"'

total=$(grep -c . "$OUT/summary.txt")
bad=$(grep -vc 'exit=0' "$OUT/summary.txt")
echo "sweep done: $total runs, $bad with failures or errors; see $OUT/summary.txt"
[ "$bad" -eq 0 ]
