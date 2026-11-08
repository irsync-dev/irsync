# Testing

## Receiving a changed file

The properties described in
[Receiving a changed file](../explanation/receiving-a-changed-file.md) are
pinned by end-to-end syncs between two engines in `crates/irsync/tests/`. They
read the receiver's event log (`SideHooks::event`, behind the `test-hooks`
feature), which records `prehash`, `plan`, `chunklist`, `need`, `chunk` (a
network chunk written), `copied` (the local copy task finished), `basis`,
`ops` (a chunk verified from operations), `identical`, `clone`, `inplace` and
`append`, each with the path. `delta.rs` checks the order of the steps (the
request leaves before the local copies, which a 150 ms delay per copy makes
visible), a stale local chunk that is re-requested, and that the old file is
hashed once from plan time; `sub_chunk.rs` tests the block codec in
`irsync::delta` on its own; `sub_chunk_delta.rs` measures the bytes of the
sub-chunk delta and its fallbacks; `inplace.rs` covers the cloned `.part`,
`--inplace` and `--append` (the clone tests skip, and say so, where `target/`
is on a filesystem that cannot clone); `content_equal.rs` covers settling in
place and `-c`; `warm.rs` covers same-size candidates and `irsync index`;
`recv.rs` holds the writer-level tests; `plan.rs` pins the planner's
`--append` and `--append-verify` decisions.

To find the code for a step: the early hash is the `Delta` arm of
`session/planner.rs`; opening, comparing, partitioning and choosing bases are
in `session/recv_io.rs` (`open_writer`, `unchanged_file`, `local_sources`,
`basis_specs`, `offer_bases`, `rebuild`, `copy_local_chunks`, `finish_unit`,
`settle_identical`, `finalize`, the in-place `Gate`); dispatch and re-requests
are in `session/receiver.rs` (`on_opened`, `on_drained`, `MAX_REREQUESTS`);
the sender's use of a basis is `session/sender_role.rs` (`on_basis`) and
`send.rs` (`payload`); the codec is `delta.rs`; the writer, cloning and
adoption are `recv.rs` (`FileWriter`, `reflink`); the folded whole-file hash
is `tree_hash.rs`; chunk lists in parts are `session/parts.rs`; the test hooks
are the fields of `SideHooks` in `session/mod.rs`.
