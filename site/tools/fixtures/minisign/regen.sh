#!/bin/sh
# Remakes the test key, the fake installer and its signatures for
# site/tools/test_install.py. Needs minisign 0.10 or newer.
# Test key only: no password, and the secret key is public. It is not a
# release key. Run it in this directory; the key and signatures all change.
set -eu
cd "$(dirname "$0")"
rm -f test.key test.pub irsync-installer.sh ./*.minisig
minisign -G -W -f -p test.pub -s test.key
printf '#!/bin/sh\necho "fake installer ran: $*"\n' > irsync-installer.sh
minisign -S -s test.key -m irsync-installer.sh -x irsync-installer.sh.minisig -t 'irsync v9.9.9'
minisign -S -s test.key -m irsync-installer.sh -x irsync-installer.sh.v9.9.8.minisig -t 'irsync v9.9.8'
# Legacy (not prehashed): must be refused.
minisign -S -l -s test.key -m irsync-installer.sh -x irsync-installer.sh.legacy.minisig -t 'irsync v9.9.9'
