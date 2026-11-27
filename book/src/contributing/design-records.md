# Design records

The book describes irsync as it is. Why it is that way is recorded in
[`docs/`](../../../docs) in the repository: one decisions file per component, each entry
naming what was decided, why, and what it costs if the ruling turns out to
be wrong. Where a decisions file and the code disagree, the code is what
shipped.

| Record | What it holds |
| --- | --- |
| [IRSYNC_DECISIONS.md](../../../docs/IRSYNC_DECISIONS.md) | irsync's design and implementation decisions that the code does not show (its numbered rulings), and the points knowingly left open. |
| [FILESET_DECISIONS.md](../../../docs/FILESET_DECISIONS.md) | The rulings made while building `fileset`. |
| [FILEGEN_DECISIONS.md](../../../docs/FILEGEN_DECISIONS.md) | The rulings made while building `filegen`. |
| [IRSYNC_HARNESS_DECISIONS.md](../../../docs/IRSYNC_HARNESS_DECISIONS.md) | The rulings made while building `irsync-stress`, the irsync bugs the harness found, its acceptance run and its sweeps. |
| [NETSTRESS_DECISIONS.md](../../../docs/NETSTRESS_DECISIONS.md) | The calls made while building `irsync-netstress` and its fault layer, and the results of the real runs. |
| [SUPPLY_CHAIN_DECISIONS.md](../../../docs/SUPPLY_CHAIN_DECISIONS.md) | How the way from a commit to a user's install is guarded, and the trade-offs taken. |

[`docs/superpowers/`](../../../docs/superpowers)
is history: the specs ([`docs/superpowers/specs/`](../../../docs/superpowers/specs)), implementation plans ([`docs/superpowers/plans/`](../../../docs/superpowers/plans)) and
progress ledgers ([`docs/superpowers/ledgers/`](../../../docs/superpowers/ledgers)) that features were built from. They are not
updated as the code changes; read them for the reasoning at the time, and
the book and the code for the current state.

Open ideas, in no particular order, are in
[TODO.md](../../../TODO.md).
