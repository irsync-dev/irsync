#!/bin/sh
# A stand-in archiver for tools/typecheck-target.sh: it writes an empty
# archive in place of the one asked for.
# MSVC targets call it as lib.exe: -out:FILE or /OUT:FILE.
for arg in "$@"; do
    case $arg in
        -out:* | -OUT:* | /out:* | /OUT:*)
            printf '!<arch>\n' >"${arg#*:}"
            exit 0
            ;;
        *.a)
            printf '!<arch>\n' >"$arg"
            exit 0
            ;;
    esac
done
