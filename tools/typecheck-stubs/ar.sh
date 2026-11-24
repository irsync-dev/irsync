#!/bin/sh
# A stand-in archiver for tools/typecheck-target.sh: it writes an empty
# archive in place of the one asked for.
for arg in "$@"; do
    case $arg in
        *.a)
            printf '!<arch>\n' >"$arg"
            exit 0
            ;;
    esac
done
