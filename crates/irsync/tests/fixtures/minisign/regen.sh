#!/bin/sh
# Remakes the test keys and signatures. Needs minisign 0.10 or newer.
# Test keys only: no password, and the secret keys are public.
# Run it in this directory. Every key and signature changes, so the unit
# tests in src/signature.rs must still pass afterwards.
set -eu
cd "$(dirname "$0")"
rm -f a.key a.pub b.key b.pub data.txt ./*.minisig
minisign -G -W -f -p a.pub -s a.key
minisign -G -W -f -p b.pub -s b.key
printf 'irsync signature test data\n' > data.txt
minisign -S -s a.key -m data.txt -x data.txt.a.minisig -t 'irsync v1.2.3'
minisign -S -s b.key -m data.txt -x data.txt.b.minisig -t 'irsync v1.2.3'
minisign -S -s a.key -m data.txt -x data.txt.old.minisig -t 'irsync v1.2.2'
minisign -S -l -s a.key -m data.txt -x data.txt.legacy.minisig -t 'irsync v1.2.3'
# Corrupt: the signature line is cut short.
awk 'NR==2 { print substr($0, 1, 40); next } { print }' data.txt.a.minisig > data.txt.corrupt.minisig
