# Retention holds and how they are released

Trusted `Runtime`/`Replica` path. Read
[checkpoints, pruning and rebootstrap](checkpoints.md) first.

This guide is for an application or host author whose domain stopped pruning,
or whose writes fail with `Error::Retention`. Two things hold retained history:
a strong request that still needs the state of its sealed cut, and a delivery
target that has not acknowledged what would be pruned. Neither is released
automatically by default.

## An unresolved strong request

A strong request without an applied result needs the complete state at its
sealed cut, so no checkpoint is made past it. `retention_status().hold` is then
`CheckpointHold::UnresolvedRequest`. Without checkpoints nothing is pruned, and a
long coordinator outage ends with writes failing at the byte bound.

Release a request explicitly:

```rust
replica.release_command(request.id).await?;
```

Or let the next checkpoint release requests that held longer than a limit:

```rust
RetentionPolicy { request_hold: Some(Duration::from_secs(24 * 3600)), ..Default::default() }
```

The time counts from when this replica stored the request, on its own clock
(`rocky_request_holds`).

A released request:

- no longer holds checkpoints, and `strong_status` reports it as
  `CommandState::Released` (`released` in the SDK);
- is skipped by `execute_pending`, so it does not block later requests;
- is not rejected or cancelled. The coordinator never heard of a release. A
  replica that kept the cut, such as a new executor after a takeover, can still
  execute it.

Once a checkpoint passes its cut, this replica can no longer execute the request
or verify its result. If a result for it arrives anyway, it stays pending until
a newer checkpoint that includes the result is imported, and strong results
after it wait too.

A request with a submitted attempt whose outcome is unknown is never released:
`release_command` returns `Rejected`, and `request_hold` skips it. Its result
may already be confirmed and must then be verified at its cut. Reconcile it with
`execute_pending` once the coordinator is reachable. During an outage
`execute_pending` fails at its first lookup, before submitting anything, so
only an attempt that was in flight when the outage began is affected.

Storing the cut's rows when a request is sealed would remove this hold
altogether; see [stored cut rows](../optimizations_on_demand/stored-cut-rows.md).

## A delivery target that does not acknowledge

The horizon passes an operation only when every configured delivery target has
acknowledged it or is declared behind. A device that never returns therefore
holds history until the byte bound stops writes.

Declare it behind explicitly with `declare_behind(peer)`, or set a time limit:

```rust
RetentionPolicy { behind_after: Some(Duration::from_secs(7 * 24 * 3600)), ..Default::default() }
```

A target is timed from the first prune step in which it holds the horizon back.
When the limit passes, it is declared behind as if by `declare_behind`, and
`retention_status().behind` lists it. The time is stored with the retention
state and survives a restart. A target that stops holding back, or names itself
in `catch_up`, starts again from zero.

A peer declared behind loses nothing it holds: its unsent operations stay in its
outbox. It learns from `catch_up` that it must import a checkpoint, and its own
edits are reapplied after the import; see
[rebootstrap](checkpoints.md#rebootstrap). Choose the limit from how long a
device may be offline before a full checkpoint transfer is cheaper than keeping
its history.
