# irsync-netstress

`irsync-netstress` runs irsync-stress's sync-and-check loop over a simulated
bad network: a fault layer compiled into irsync with the cargo feature
`netsim` shapes every QUIC packet between client and daemon by a named
profile (`4g-vehicle`, `dsl-video`, `long-rtt`), with limited and changing speed, queues,
delay, bursty loss and dead zones that force a reconnect and resume. It needs
a separate build, `cargo build --release --workspace --features netsim
--target-dir target/netsim`. Usage, profiles, output and exit codes are in
[Network stress with netstress](https://irsync.org/book/contributing/netstress.html),
the inside of the fault layer in
[How netstress works](https://irsync.org/book/contributing/netstress-internals.html).

`--concurrent` runs the same loop with filegen writing to the source during
each sync, and judges every destination file against the states the source
held. See
[Writing during the sync](https://irsync.org/book/contributing/bug-hunting.html#writing-during-the-sync---concurrent).
