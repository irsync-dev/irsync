#!/bin/bash
# The campaign matrix of book/src/contributing/bug-hunting.md: one breadth.sh
# sweep per row, each aiming filegen or the client at one part of irsync.
# Every passing run gets clean-check.sh (paranoid check, no .part left over).
#
#   crates/stress/tools/matrix.sh [ROW...]     (default: every row, in order)
#
# Rows: hardlink rename rename-off sparse metadata timestamps symlinks deletion
#   listing longnames parallel1 parallel512 checksum minimal bigfile paranoid
#
# Environment (defaults in brackets):
#   BIN   directory holding irsync-stress and the tools  [target/release]
#   BASE  one breadth.sh OUT per row goes below it        [$HOME/work/irsync-matrix-<date>]
#   SEEDS seeds for the rows that do not set their own    [101 120]
#   DRY   if set, only check that filegen accepts every row's settings
#
# Rows set their own run size where it matters (see row() below); the rest
# use breadth.sh's defaults. `bigfile` needs up to 4 x 2 x 16G of disk.
# Rows missing on purpose: --max-delete (the harness counts client exit 25
# as its own error), xattrs (irsync has no -X). The checksum row reuses
# seeds 1 to 20, the default sweep's, so a difference points at -c.
set -u
TOOLS=$(cd "$(dirname "$0")" && pwd)
BIN=${BIN:-$(cd "$TOOLS/../../.." && pwd)/target/release}
BASE=${BASE:-$HOME/work/irsync-matrix-$(date +%Y-%m-%d)}
export BIN
KINDS="create_file append truncate overwrite_range rewrite punch_hole delete_file mkdir rmdir
  remove_tree rename rename_over symlink repoint_symlink delete_symlink hardlink mkfifo chmod
  set_mtime set_xattr chown mknod"

# col KIND WEIGHT: column KIND set to WEIGHT in every row of filegen's transition matrix.
col() { for k in $KINDS; do printf -- '--filegen-set weights.%s.%s=%s ' "$k" "$1" "$2"; done; }

# A row is "ENV OVERRIDES|irsync-stress ARGS".
row() {
  case $1 in
    hardlink)    echo "|$(col hardlink 10) $(col rename 8) $(col rename_over 8)" ;;
    rename)      echo "|$(col rename 15) $(col rename_over 10) $(col overwrite_range 5)" ;;
    rename-off)  echo "CLIENT='--timeout 300 --no-rename-detection'|$(col rename 15) $(col rename_over 10) $(col overwrite_range 5)" ;;
    sparse)      echo "CLIENT='--timeout 300 -S'|$(col punch_hole 10) --filegen-set content.pattern_fraction=0.5" ;;
    metadata)    echo "|$(col chmod 10) --filegen-set chmod.special_weight=0.5" ;;
    timestamps)  echo "|--filegen-set mtime.recent=0.3 --filegen-set mtime.distant=0.2 --filegen-set mtime.future=0.5 --filegen-set mtime.future_years=50 --filegen-set mtime.distant_years=70" ;;
    symlinks)    echo "|$(col repoint_symlink 10) --filegen-set symlink.dangling=0.5 --filegen-set symlink.absolute=0.5" ;;
    deletion)    echo "CLIENT='--timeout 300 --delete-before'|$(col remove_tree 5) --filegen-set remove_tree_max_entries=5000" ;;
    listing)     echo "ITER=10 STEPS=5000 FILE=8M|--filegen-set size_distribution.small.weight=0.95 --filegen-set max_depth=64 --filegen-set names.classes.unicode=30 --filegen-set names.classes.awkward=30" ;;
    longnames)   echo "|--filegen-set names.classes.long=30 --filegen-set max_depth=64" ;;
    parallel1)   echo "CLIENT='--timeout 300 --parallel 1'|" ;;
    parallel512) echo "CLIENT='--timeout 300 --parallel 512' ITER=10 STEPS=5000 FILE=8M|--filegen-set size_distribution.small.weight=0.95" ;;
    checksum)    echo "CLIENT='--timeout 300 -c' SEEDS='1 20'|" ;;
    paranoid)    echo "CLIENT='--timeout 300 --paranoid'|" ;;
    minimal)     echo "ITER=200 STEPS=1 SEEDS='101 108'|" ;;
    bigfile)     echo "CLIENT='-c --timeout 10' ITER=5 STEPS=20 TOTAL=16G FILE=8G SEEDS='101 104' JOBS=4|--filegen-set size_distribution.large_weight=1" ;;
    *) echo "unknown row: $1" >&2; return 1 ;;
  esac
}
ALL="hardlink rename rename-off sparse metadata timestamps symlinks deletion listing longnames
  parallel1 parallel512 checksum minimal bigfile paranoid"
ROWS=${*:-$ALL}

# A one-step dry run per row: a typo in a setting fails here, not mid-campaign.
for r in $ROWS; do
  spec=$(row "$r") || exit 2
  sets=$(echo "${spec#*|}" | sed 's/--filegen-set /--set /g')
  t=$(mktemp -d)
  mkdir "$t/tree"
  # shellcheck disable=SC2086  # sets is a word list by design
  if ! "$BIN/filegen" run --seed 1 --steps 1 --dry-run --out-dir "$t/out" $sets "$t/tree" > "$t/log" 2>&1; then
    echo "row $r: filegen rejects its settings:" >&2
    cat "$t/log" >&2
    rm -rf "$t"
    exit 2
  fi
  rm -rf "$t"
done
echo "settings ok for:" $ROWS
[ -n "${DRY:-}" ] && exit 0

mkdir -p "$BASE"
for r in $ROWS; do
  spec=$(row "$r")
  envs=${spec%%|*}; args=${spec#*|}
  echo "=== $r  start $(date +%H:%M:%S)  env: ${envs:-defaults}"
  eval "env OUT='$BASE/$r' SEEDS='${SEEDS:-101 120}' CHECK='$TOOLS/clean-check.sh' $envs \
    '$TOOLS/breadth.sh' $args" | tail -1
done
echo "=== matrix done $(date +%H:%M:%S)"
bad=0
for r in $ROWS; do
  n=$(grep -vc 'exit=0' "$BASE/$r/summary.txt")
  printf '%-12s runs=%s bad=%s\n' "$r" "$(grep -c . "$BASE/$r/summary.txt")" "$n"
  bad=$((bad + n))
done
[ "$bad" -eq 0 ]
