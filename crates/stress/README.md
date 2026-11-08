# irsync-stress

`irsync-stress` runs irsync's stress loop on loopback: it starts an irsync
daemon, fills a source tree with `filegen`, then repeatedly syncs it with the
irsync client, compares the two `fileset` manifests, checks the destination
with `fileset check` and mutates the source again, keeping logs, both
manifests and a `run.toml` for every failure. The command line, output,
layout, exit codes and the campaign scripts in `tools/` are in
[Stress testing](https://irsync.org/book/contributing/stress.html); how to
make it find bugs is in
[Hunting bugs](https://irsync.org/book/contributing/bug-hunting.html).
