#!/usr/bin/env python3
"""A stand-in for minisign, for the release script tests.

Every call is appended, as a JSON list, to the file named by
FAKE_MINISIGN_LOG.

If FAKE_MINISIGN_REAL names a real minisign, the call is passed on to it.
Otherwise this script signs and verifies by itself. Its signatures are in
minisign's file format and tied to the key pair, but they are sha512 hashes,
not Ed25519: a signature made with one key does not verify with another, and
a changed file or trusted comment does not verify either. That is enough to
test what the scripts do with minisign without needing it installed.

Only the calls the scripts make are understood:
  -S [-s KEY] -t COMMENT -m FILE...   writes FILE.minisig for each FILE
  -V [-H] -Q -p PUBKEY -m FILE        prints the trusted comment (a legacy
                                      signature always fails, as with -H)
"""
import base64
import hashlib
import json
import os
import sys

args = sys.argv[1:]
with open(os.environ["FAKE_MINISIGN_LOG"], "a") as f:
    f.write(json.dumps(args) + "\n")

real = os.environ.get("FAKE_MINISIGN_REAL")
if real:
    os.execv(real, [real] + args)


def fail(msg):
    print(msg, file=sys.stderr)
    sys.exit(1)


def opt(name):
    return args[args.index(name) + 1] if name in args else None


def key_line(path):
    with open(path) as f:
        lines = f.read().splitlines()
    if len(lines) < 2 or not lines[0].startswith("untrusted comment:"):
        fail(f"{path}: not a minisign key file")
    return base64.b64decode(lines[1])


def file_hash(path):
    with open(path, "rb") as f:
        return hashlib.sha512(f.read()).digest()


def sign_bytes(pk, data):
    return hashlib.sha512(pk + data).digest()


if args[0] == "-S":
    sk = key_line(opt("-s") or os.path.expanduser("~/.minisign/minisign.key"))
    # The unencrypted secret key (minisign -G -W): the key id at 54, then the
    # Ed25519 secret key, whose second half is the public key.
    keyid, pk = sk[54:62], sk[94:126]
    comment = opt("-t")
    files = args[args.index("-m") + 1:]
    for name in files:
        sig = sign_bytes(pk, file_hash(name))
        glob = sign_bytes(pk, sig + comment.encode())
        with open(name + ".minisig", "w") as f:
            f.write("untrusted comment: signature from fake minisign\n"
                    + base64.b64encode(b"ED" + keyid + sig).decode() + "\n"
                    + f"trusted comment: {comment}\n"
                    + base64.b64encode(glob).decode() + "\n")
elif args[0] == "-V" and "-Q" in args:
    pub = key_line(opt("-p"))
    keyid, pk = pub[2:10], pub[10:42]
    name = opt("-m")
    with open(name + ".minisig") as f:
        lines = f.read().splitlines()
    raw = base64.b64decode(lines[1])
    comment = lines[2].removeprefix("trusted comment: ")
    if raw[:2] != b"ED":
        fail("legacy signature")
    if raw[2:10] != keyid:
        fail(f"Signature key id in {name}.minisig is {raw[2:10].hex()}\n"
             f"but the key id in the public key is {keyid.hex()}")
    if raw[10:] != sign_bytes(pk, file_hash(name)):
        fail("Signature verification failed")
    if base64.b64decode(lines[3]) != sign_bytes(pk, raw[10:] + comment.encode()):
        fail("Comment signature verification failed")
    print(comment)
else:
    fail(f"unexpected call {args}")
