# fileset

`fileset` records the state of a directory tree in a manifest stored at the
root of that tree, and later verifies the tree against it: run `init` once,
run `check` months later, and learn whether any file was lost, any byte
flipped or any metadata drifted. It also validates irsync independently, with
code that shares nothing with irsync: `init` on the client's tree, let irsync
carry the tree and its manifest to the daemon, `check` there. Install it with
`cargo install --git https://github.com/irsync-dev/irsync fileset`. Commands,
options, output and exit codes are in the
[fileset reference](https://irsync.org/book/fileset/reference.html).
