# Encrypted checkpoints and pruning

`EncryptedReplica` path. The trusted path shares no code or format; see
[checkpoints](checkpoints.md). Finalizing rows at an issuer-signed cut is in
[mergeable horizon](mergeable-horizon.md); the statement and rationale are at the
[end](#the-signed-statement).

A scope keeps its stored rows, a signed checkpoint of them, and the operations
after a per-log *horizon*. This guide is for an application or transport author
who configures retention, syncs a peer that was offline, or verifies a file.

## What runs on its own

After each group commit the replica runs one bounded step for the scopes it
wrote: continue or start a checkpoint, otherwise prune one batch. A checkpoint
refreshes when its policy or key epoch changes, even without new operations. It
starts after `checkpoint_operations` applied operations, or earlier once
retained bytes reach `soft_bytes`. `checkpoint(scope)` and `prune(scope)` run the
same steps to completion. `checkpoint_status(scope)` reports the checkpoint,
covered and uncovered operations, the horizon per log, retained bytes and the
last `maintenance_error`.

| `RetentionLimits` field | Default | Meaning |
| --- | --- | --- |
| `checkpoint_operations` | 1,024 | Applied operations after the last checkpoint that start the next |
| `soft_bytes` | 32 MiB | Retained header and body bytes that start checkpointing and pruning |
| `hard_bytes` | 64 MiB | Retained bytes at which `author` and `receive` return `Error::Backpressure` |
| `handles` | 65,536 | Newest mutation handles kept after their operations are pruned |

Limits are per open replica and not persisted: call `set_retention_limits` after
every open. A replica without a key for a scope holds custody only; it cannot
verify a checkpoint, so it never prunes and eventually reports backpressure.

The SDK installs configurable [default policies](sdk-bootstrapping.md#retention)
for its granted collections. Direct runtime consumers still configure their own.

## When pruning waits

Nothing is pruned until `set_retention_policy(scope, policy)` names who must
hold copies. `prune` reports what stopped it as a `PruneBlock`:

| `PruneBlock` | Cause |
| --- | --- |
| `NoCheckpoint` | No verified checkpoint, or it no longer verifies |
| `NoPolicy` | No `RetentionPolicy` for the scope |
| `RecoveryCustody` | The domain requires recovery custodians, whose packages carry complete history |
| `Unacknowledged` | A `required` peer not listed in `behind` has not acknowledged the operation |
| `HorizonCustody` | Finalized existence bases lack enough other holders with a cut acknowledgment or finalized bootstrap receipt |
| `SoleCopy` | Fewer than `max(1, copies)` other nodes acknowledged it; the last copy is never released |
| `Custody` | Fewer configured full nodes than the [role](replica-roles.md) requires have applied it |

Pruning deletes headers, bodies, acknowledgments and outbox entries in atomic
batches of 64, oldest first, and only behind a verified, flushed checkpoint.

The newest `window` covered operations always stay, for peers that are slightly
behind. `export_recovery` refuses a scope with a mergeable operation horizon;
a strong horizon instead travels as a base with its retained suffix.

## Syncing a peer

`NativeSession::pull_page` returns the frames and, in `rebootstrap`, the scopes
whose history the peer pruned past what this replica holds; their frames are not
received. `pull` fails for such a scope instead. Without a session, compare
`horizon_page` of the source with `behind_horizon` on the target.

A frame at or before the local horizon is answered `ReceiveStatus::Pruned` and
not stored. Ordinary checkpoint pruning keeps tombstones, so nothing recreates a
deleted row; only a [mergeable horizon](mergeable-horizon.md) removes them.

## Rebootstrap

SDK [join](sdk-bootstrapping.md#join) requests a checkpoint before ordinary
catch-up, even when the source still retains its history. A writable source
seals a current checkpoint on demand when offering it. `bootstrap_scope` performs
that import followed by tail catch-up; `sync_scope` keeps using the behind signal.

`NativeSession::rebootstrap(replica, scope, quota)` fetches the peer's signed
checkpoint and its rows in pages and switches to it. An interrupted import of
the same checkpoint continues from `rebootstrap_progress`. The SDK reports
[bootstrapping readiness](sdk-bootstrapping.md#readiness) while import remains. Without a session,
use `checkpoint_offer`, `begin_rebootstrap`, `checkpoint_page`,
`stage_checkpoint_page` and `finish_rebootstrap`.

Pages also carry confirmed command rows and the latest result under the current
scope key; the switch installs their sequence/head as the
[strong base](strong-history.md#read-and-catch-up). A cached offer behind the
source's strong pruning base is refreshed.

The switch verifies the staged rows against the statement. `RebootstrapReport`
accounts for every local operation: `superseded` when the checkpoint covers it,
otherwise `reapplied`, `rejected` with the reason, or `pending` on a dependency
or key. A checkpoint that ends before the replica's own horizon, or whose
signature, signer or definition does not verify, is refused. The signer needs
whole-scope read and a write grant; see
[bootstrap signers](sdk-bootstrapping.md#bootstrap-signers). Its credentials
must be valid at admission and again at the final switch; an interrupted import
does not pin expired authority.

## Retries

A mutation handle whose operation was pruned is not authored again: `author`
returns `Rejected`, while `author_buffered` and `lookup_mutation` return the
operation hash. Reusing it with other input returns `Conflict`. Pruning forgets
the oldest handles beyond `handles` whose operations are gone; a retry of one of
those authors a new operation. Strong history uses a [per-domain base and window](strong-history.md): settled
commands and redundant preparations are released, and results whose jobs have
finished are pruned after required readers apply them. The latest result stays.
A retained marker prevents a pruned command handle from being signed again;
retry returns `Error::Pruned`, and other input returns `Conflict`. Recovery
transfers carry these markers. Effect observations remain retained.

## Verifying a file

Open replays only the operations after each scope's verified checkpoint and
compares the result with the stored rows and digest. `audit(scope)` checks every
retained operation, the counters, handles, stored rows and checkpoint rows
against their digests, and the replay, in batches under the replica lock.

## Limits

- Surviving rows are stored twice. A creation stub (hash, clock; about 45 bytes)
  stays per stored row, tombstones included, until a horizon finalizes it.
  Handles cost about 200 bytes each up to the window.
- A frame naming a pruned dependency other than its log predecessor or a row
  creation stays pending until a late copy of it arrives or a newer checkpoint
  is imported.
- The rebootstrap switch and `rebuild_projection` are one transaction
  proportional to the scope's rows.
- A checkpoint names at most 256 logs per scope and must fit one 256 KiB frame.
- `export_recovery` and recovery custodians need complete mergeable history.

## Offering an imported checkpoint back to its signer

A replica refuses to rebootstrap from its own signed checkpoint. An authorized
writer holding an imported checkpoint therefore creates a locally signed cut
before offering it back to the original signer, even when no row has changed.
Retries keep that new offer stable. A read-only holder cannot replace the
signature and offers no checkpoint to the original signer; it can still serve
the imported cut to other authorized peers. Signature, write-grant, generation,
key-epoch and transactional import checks remain enforced.

## The signed statement

A checkpoint covers one `ResourceScope`. `SignedCheckpoint` is a
`CheckpointStatement`, the signer's node key and credential chain, signed under
`rocky.encrypted-checkpoint.v3`; its identity is the
`rocky.encrypted-checkpoint.id.v3` commitment of its bytes. `format` is 6.
Partition candidates, support rows and hidden marks are derived locally. A
partition checkpoint carries only its authenticated tuple, materialized rows and
identity marks; rebootstrap rebuilds its group partition and leaves other
partitions unchanged. See [decision 0043](../decisions/0043-partition-horizon-state.md).

Partition, admitted horizon and finalized horizon are independent optional
fields; an admitted cut is never treated as finalized. Other formats are
refused before writes.

| Field | Binds |
| --- | --- |
| `definition`, `schema`, `handler` | Manifest hash and generations |
| `policy_epoch`, `key_epoch`, `scope`, `partition` | Policy epoch at signing, the scope and its key generation |
| `sequence`, `previous` | Position in the scope's checkpoint chain and the prior identity |
| `frontier`, `clock`, `operations` | Per author applied sequence, tip hash and clock, ordered by author; highest clock; applied count |
| `identities` | Per-author identity high-water marks |
| `rows`, `tombstones` | Count and digest of every stored row record, tombstones and creation hashes included |
| `creations`, `handles` | Count and digest of (creation hash, clock) of stored rows, and of retained mutation handles |
| `strong` | Base sequence/head and command-row commitment; retained results, commands, preparations and observations by identity |
| `custody` | Covered operations others acknowledged, sole copies and recovery archives held |
| `horizon`, `finalized` | Admitted and finalized [horizon](mergeable-horizon.md) declarations |

Creation is capture (frontier, counts and live digest in one transaction),
replay of retained frames into an overlay over the previous checkpoint (64
operations per transaction), seal (the replayed digest must equal the captured
one, else the previous checkpoint stays and pruning stops there) and install
(256 rows per transaction).

## Design rationale

- **Per scope**, because keys, read authority and custody are per scope.
- **Set digests are LtHash16 with 1,024 lanes** (SHA-256 of the state in the
  statement): members can be added and removed, so the digest is maintained in
  every applying transaction and a checkpoint never scans a scope. Rejected: a
  plain sum of 256-bit hashes (collisions at about 2^32 work, and peers here are
  adversarial) and a streamed hash (one pass over the scope per checkpoint).
- **Verified by recomputation**: a checkpoint is used only after "previous
  checkpoint plus retained operations" reproduced the stored projection. The
  local application order is causally closed, so any prefix is a consistent cut.
- **A handle window**, not a residue per authored operation, so the file stops
  growing; a retry older than the window authors again.
- **The last copy is never released** and a custody-only replica never prunes:
  it cannot verify what a checkpoint claims.
- Open: may a custody-only replica prune under a checkpoint signed by a reader a
  policy names as trusted, and should recovery packages carry a checkpoint so
  that domains with required custodians can prune?
