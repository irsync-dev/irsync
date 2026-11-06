#!/bin/bash
# paranoid-check.sh, then assert that no irsync staging file (.irsync.*.part)
# is left in either tree of a finished irsync-stress root. For breadth.sh's
# CHECK hook, or by hand on any run root.
#
#   clean-check.sh <ROOT>
set -u
root=${1:?usage: clean-check.sh <ROOT>}
"$(dirname "$0")/paranoid-check.sh" "$root" || exit 1
left=$(find "$root/DST/data" "$root/SRC/data" -name '.irsync.*.part' 2>/dev/null)
if [ -n "$left" ]; then
  echo "$left"
  echo "leftover .part files: $(echo "$left" | wc -l)"
  exit 1
fi
