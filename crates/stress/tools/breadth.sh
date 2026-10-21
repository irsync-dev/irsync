#!/bin/bash
# Breadth sweep from docs/STRESS_BUG_HUNTING.md: many seeds, short runs, both
# directions, several at a time. Roots of passing runs are deleted (a pass is
# reproducible from its seed); roots of failing runs are kept for diagnosis.
#
#   crates/stress/tools/breadth.sh [EXTRA irsync-stress ARGS...]
#
# Environment (defaults in brackets):
#   BIN   directory holding irsync-stress and the tools  [target/release]
#   OUT   where roots, per-run logs and summary.txt go    [$HOME/work/irsync-breadth]
#   SEEDS seeds to run, a seq(1) expression              [1 50]
#   JOBS  runs at a time                                 [8]
#   ITER, STEPS, TOTAL, FILE  size of each run           [20, 200, 4G, 256M]
#
# Each run needs up to 2 x TOTAL on disk while it runs; JOBS runs at once.
# Extra arguments go to irsync-stress before the root, e.g.
#   breadth.sh --filegen-set weights.create_file.hardlink=10
set -u
BIN=${BIN:-$(cd "$(dirname "$0")/../../.." && pwd)/target/release}
OUT=${OUT:-$HOME/work/irsync-breadth}
SEEDS=${SEEDS:-1 50}
JOBS=${JOBS:-8}
ITER=${ITER:-20}; STEPS=${STEPS:-200}; TOTAL=${TOTAL:-4G}; FILE=${FILE:-256M}
EXTRA=("$@")
for t in irsync-stress irsync filegen fileset; do
  [ -x "$BIN/$t" ] || { echo "missing $BIN/$t (cargo build --workspace --release)" >&2; exit 2; }
done
mkdir -p "$OUT"
export BIN OUT ITER STEPS TOTAL FILE
export EXTRA_ARGS="${EXTRA[*]}"

run() {
  dir=$1; seed=$2; root=$OUT/$dir-$seed
  # shellcheck disable=SC2086  # EXTRA_ARGS is a word list by design
  "$BIN/irsync-stress" --direction "$dir" --seed "$seed" --iterations "$ITER" --steps "$STEPS" \
    --max-total-size "$TOTAL" --max-file-size "$FILE" --keep-going $EXTRA_ARGS "$root" \
    -- --timeout 300 > "$OUT/$dir-$seed.out" 2>&1
  rc=$?
  echo "$dir $seed exit=$rc $(tail -1 "$OUT/$dir-$seed.out")" >> "$OUT/summary.txt"
  [ $rc -eq 0 ] && rm -rf "$root"
}
export -f run

: > "$OUT/summary.txt"
# shellcheck disable=SC2086  # SEEDS is "first last" for seq
for seed in $(seq $SEEDS); do for dir in push pull; do echo "$dir $seed"; done; done \
  | xargs -P "$JOBS" -n 2 bash -c 'run "$0" "$1"'

total=$(grep -c . "$OUT/summary.txt")
bad=$(grep -vc 'exit=0' "$OUT/summary.txt")
echo "sweep done: $total runs, $bad with failures or errors; see $OUT/summary.txt"
[ "$bad" -eq 0 ]
