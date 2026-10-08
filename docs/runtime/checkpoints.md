# Checkpoints, pruning and rebootstrap

Trusted `Runtime`/`Replica` path. The encrypted path shares no code or format;
see [encrypted checkpoints](encrypted-checkpoints.md). Rationale is at the
[end](#design-rationale).

A domain no longer keeps every operation. It keeps the stored projection, a
verified checkpoint of it, and the operations after a *horizon*. This guide is
for an application or transport author who has to configure retention, sync a
peer that was offline, or verify a file.

## What runs on its own

With the default `RetentionPolicy`, the file writer runs one bounded maintenance
step beside each group of writes of a domain:

1. After 4,096 applied operations, or at half the byte bound, it records a
   checkpoint candidate at the applied frontier.
2. It replays retained operations into the checkpoint copy, about twice as many
   as the group wrote, until the copy reaches the candidate.
3. The copy's row digest must equal the projection's digest at that frontier.
   The checkpoint is then verified and the writer flushes once.
4. The horizon moves up to the checkpoint, less the retained window, as far as
   delivery targets allow. Operations behind it are deleted in batches.

Nothing runs while a domain is idle. `Replica::checkpoint()` and
`Replica::prune()` run the same steps to completion on request, and are the only
way with `automatic: false`.

| `RetentionPolicy` field | Default | Meaning |
| --- | --- | --- |
| `checkpoint_operations` | 4,096 | Applied operations after the last checkpoint that start the next. Zero: only on request or at half the byte bound |
| `window` | 1,024 | Most recent applied operations per author kept behind a checkpoint |
| `retained_bytes` | 64 MiB | Hard bound on retained operation bytes, and separately on strong ledger bytes |
| `handles` | 65,536 | Most recent local mutation and command handles kept for retries |
| `automatic` | true | Run steps beside writes |
| `request_hold` | None | How long an unresolved strong request may hold checkpoints before it is released |
| `behind_after` | None | How long a delivery target may hold back the horizon before it is declared behind |
| `custody` | 0 | Full nodes that must have applied an operation before the horizon passes it; zero waits for every delivery target |

Set it in `RuntimeOptions::retention`. `Replica::open_with_options` takes options
for a single-domain file.

## When pruning waits

The horizon passes an operation only when every configured delivery target has
acknowledged it, has reported it applied in `catch_up`, or has been declared
behind. An operation nobody else is known to hold is never deleted.

`Replica::retention_status()` reports the checkpoint, horizon, retained counts
and a hold:

| `CheckpointHold` | Cause | What to do |
| --- | --- | --- |
| `UnresolvedRequest` | A strong request has no applied result and needs the state at its sealed cut | Execute or deliver the result, or [release it](retention-holds.md#an-unresolved-strong-request) |
| `Unverifiable` | The recomputed checkpoint differed from the projection | Treat the file as corrupt; import a peer's checkpoint |
| `Rebootstrap` | A checkpoint import is in progress | Finish or abort it |
| `NoBase` | An import was aborted | Import a checkpoint |

When retained bytes reach the bound, `mutate`, `receive` and strong requests fail
with `Error::Retention`. It is retryable and has the failure reason `Quota`.
Nothing is dropped. A peer that will not return is released with
`declare_behind(peer)` or after `behind_after`; that is an application policy
decision, and the peer must then import a checkpoint. See
[retention holds](retention-holds.md) for both holds and their releases.

## Syncing a peer

Use `catch_up` instead of `missing_operations`. A pass of bounded range
requests uses `catch_up_after` with the cursor in `TransferPage::next`; see
[full and partial replicas](replica-roles.md#trusted-path):

```rust
match source.catch_up(Some(peer), target.sync_state().await?).await? {
    Catchup::Operations(page) => { /* receive, acknowledge, process_pending */ }
    Catchup::BehindHorizon(notice) => { /* import the checkpoint, below */ }
}
```

`catch_up` compares the peer's applied frontier with the horizon. Run
`process_pending` on the peer first, or received operations it has not applied
yet make it look behind. Naming a delivery target records its held and applied
frontiers as acknowledgment and clears a `declare_behind`. With
`RetentionPolicy::custody`, only full nodes' applied frontiers move the horizon.

`SyncState::held` is the contiguous prefix per author; `received` lists at most
256 retained operations above it.
`missing_operations(known)` still returns retained operations absent from an
exact set, but it cannot tell a peer that pruned from one that never received,
and it cannot signal the horizon.

`HorizonNotice::encode` is the wire form of the signal (magic `rocky.horizon.v1`):
scope, node, horizon, and the commitment and sequence of the checkpoint to import.
From the moment a horizon is stored, operations at or before it count as applied.
Pruning is driven by the stored horizon alone, so it is idempotent and resumes
after a crash.

## Rebootstrap

```rust
let mut cursor = None;
loop {
    let page = source.checkpoint_page(cursor).await?;
    cursor = page.next;
    if let Some(report) = target.import_checkpoint(page).await? {
        break report;
    }
}
```

- Pages (`rocky.checkpoint.page.v1`) are bounded to 512 rows and 256 KiB; the
  first carries the record and the latest strong result. A repeated
  page is ignored; a gap is refused. If the source makes a newer checkpoint
  during the pass, `checkpoint_page` returns `Rejected`: start again without a
  cursor. An export defers automatic checkpoints for 30 seconds per page.
- Until the last page the target serves its own projection and accepts writes.
- The last page verifies the staged rows against the record, replaces the
  checkpoint tables and applies the remaining retained operations again.
  `RebootstrapReport` lists local operations as `reapplied`, `rejected` with the
  application's reason, or `pending` on a dependency that still has to arrive.
  `rejected_local_operations()` returns the rejected ones later too.
- A checkpoint that does not cover what the importer itself pruned is refused.
- Then continue with `catch_up` in both directions. The target's unsent
  operations are still in its outbox.

A rejected local operation ends that author's log on every replica, as a
rejected remote one always did: later operations depend on it. The todo and
inventory reducers do not refuse replayed operations.

`abort_rebootstrap()` drops a partial import. The replica keeps its projection
but cannot make checkpoints until an import completes.

The application declares which tables a checkpoint carries with
`InstalledSchema::checkpoint_tables` (`CompiledSchema::checkpoint_tables` in a
compiled adapter). They must hold only mergeable projection
rows, including deletion evidence such as tombstones. An application with
mergeable rows that declares no table cannot be checkpointed: the import fails
its digest check.

Each declared table is stored twice, with its indexes, and every operation is
applied to it twice. Declare only the tables a merge needs; keep caches, search
indexes and command results out. Declaring nothing to save space is not an
option, for the reason above. A leaner mode for very large applications is
[deferred](../optimizations_on_demand/lean-checkpoints.md).

## What a retry or a late operation sees

- An operation at or before the horizon that arrives again is acknowledged as an
  applied duplicate. Its bytes are not compared.
- An operation that depends on one behind the horizon is ready. A row deleted
  before the horizon stays deleted, because its tombstone is a stored row.
- A mutation or command handle whose operation or request was pruned returns
  `Error::Pruned` and is not executed. Reusing it with other input still returns
  `Conflict`. A handle older than `handles` is forgotten.

## Verifying a file

Open validates counters, the checkpoint record, the operations after the
checkpoint and the strong ledger. It does not replay history.

`Replica::audit()` verifies everything, in bounded memory:

1. Retained operations, counters, acknowledgments, handles, outgoing work and the
   strong ledger, read in pages on the writer thread without a time bound.
2. A new checkpoint, which recomputes the projection from the previous one and
   retained operations.
3. A scan of the projection and the checkpoint copy against their digests, on a
   pooled read connection in one snapshot.

It returns `Corrupt` on any difference. `AuditReport::recomputed` is false when a
hold prevented step 2. The audit writes a checkpoint, blocks the file's writer
during step 1 and keeps the WAL from being reset during step 3.

## Limits

- The checkpoint copy doubles the checkpoint tables and their rows.
- An import's last step replaces the checkpoint tables in one transaction.
- A strong command still holds the complete state of its cut in memory, now the
  checkpoint copy plus the operations after it. A result names the executor's
  base, and more than 32,768 operations after it are refused with `Quota`
  ([Decision 0042](../decisions/0042-bounded-strong-cut.md)).
- An executor whose checkpoint is past a request's cut, or that has none,
  reports `StrongPending(CutBase)`. A confirmed result whose cut is behind this
  replica's checkpoint stays pending until a newer checkpoint is imported.
- Rejection evidence and released strong requests are never pruned and count
  towards the strong ledger's byte bound.
- Per-operation acknowledgments, where a transport still sends them, are kept
  until pruned.
- An identity conflict or invalid Lamport time cannot be detected against a
  pruned operation.
- An aborted import leaves the replica without a checkpoint base. `audit` is not
  part of open: a file corrupted outside the runtime is reported only by `audit`.
- `rusqlite`'s `preupdate_hook` feature needs libclang at build time.

## Design rationale

- **The checkpoint is a second copy of the projection**, in the same file under
  a second table namespace, advanced by replaying retained operations with the
  application's own reducer. Rejected: a running digest without rows (cannot
  recompute from the previous checkpoint, cannot rebuild a strong cut, and an
  export would hold one read snapshot for the whole transfer); before-images of
  changed rows (needs a row key in the contract and still recomputes nothing).
- **Set digests** are the sum modulo 2^256 of SHA-256 row hashes plus a count,
  kept for the projection, the copy and the replayed operations, maintained by
  SQLite's pre-update hook so any installed statement is covered. They detect
  corruption and divergence, not adversarial collisions; the trusted path does
  not need that. The encrypted path uses LtHash instead.
- **Verification by recomputation**: a checkpoint is always "previous checkpoint
  plus retained operations" compared with what the projection held, in bounded
  steps; no step spans the domain.
- **Missing acknowledgments block pruning**: the peer's copy might be the only
  other one. Releasing a peer (`declare_behind`, `behind_after`) or a strong
  request (`release_command`, `request_hold`) is an explicit application policy.

The record (`CheckpointRecord::encode`, magic `rocky.checkpoint.v1`, commitment
`rocky.checkpoint.record.v1`) is canonical little-endian with length-prefixed lists:

| Field | Binds |
| --- | --- |
| `scope`, `definition`, `versions`, `projection` | Domain, reducer, schema, projection query and handler generations |
| `frontier`, `clock` | Applied frontier per author; highest Lamport time at or before it |
| `history`, `state` | Set digests of the covered operations and of the rows at the frontier |
| `node`, `policy_version`, `key_epoch` | Maker, its policy version; key epoch is always zero here |
| `sequence`, `previous` | Position in this replica's chain and the record it was recomputed from |
| `strong` | Configuration, sequence, head, latest applied result, digest of requests and preparations without a result |
| `handles` | Count and lowest retained sequence of mutation and command handles |

Deletion evidence is stored tombstone rows, so it is inside `state`.
