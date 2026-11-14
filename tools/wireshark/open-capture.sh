#!/bin/sh
# Open an irsync protocol capture in stock Wireshark, nothing configured
# permanently. Usage: open-capture.sh FILE.pcapng
set -eu
[ $# -eq 1 ] || { echo "usage: $0 FILE.pcapng" >&2; exit 2; }
here=$(cd "$(dirname "$0")" && pwd)
proto="$here/../../proto"
proto=$(cd "$proto" && pwd)
exec wireshark \
    -o protobuf.default_type:irsync.capture.v1.Record \
    -o "uat:protobuf_search_paths:\"$proto\",\"TRUE\"" \
    "$1"
