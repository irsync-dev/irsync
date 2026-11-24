#!/bin/sh
# A stand-in C compiler for tools/typecheck-target.sh: it compiles nothing
# and writes an empty file where the output was asked for.
prev=
for arg in "$@"; do
    if [ "$prev" = -o ]; then
        : >"$arg"
    fi
    case $arg in
        -o?*) : >"${arg#-o}" ;;
        --version | -dumpversion) echo 12.0.0 ;;
    esac
    prev=$arg
done
