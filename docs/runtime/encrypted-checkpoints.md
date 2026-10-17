# Encrypted checkpoints and pruning

This guide is for applications configuring encrypted history and transports
serving a device that needs a base. The trusted runtime has a separate
[checkpoint contract](checkpoints.md).

A scope keeps its stored rows, a verified signed checkpoint and operations
after that base. In a domain with a [tip graph](tips-and-bookmarks.md),
checkpoints sit at seals of the followed path. Devices finalize and compact
locally; see [lazy compaction](lazy-compaction.md) for frontiers, final seals,
late frames and losing-seal rebootstrap.

## What runs on its own

After a group commit the replica runs a bounded maintenance step for its
written scopes: it finalizes ready seals of the followed path. Every domain
has a tip graph, so no checkpoint forms at an arbitrary cut; `compact(keep)`
installs a base at a seal. Until someone compacts, a join uses frames.
Unchanged scopes keep their earlier seal base. `checkpoint(scope)` returns
the scope's base, and `prune(scope)` completes pruning. `checkpoint_status`
reports the base, covered/uncovered operations, each log's `pruned_prefix`,
retained bytes and the last `maintenance_error`.

| `RetentionLimits` field | Default | Meaning |
| --- | --- | --- |
| `hard_bytes` | 64 MiB | Retained bytes at which authoring/receipt backpressure, excluding staged publication frames |
| `handles` | 65,536 | Newest mutation handles retained after pruning |

Limits are per open replica; install them after every open. The SDK installs
[default policies](sdk-bootstrapping.md#retention). A keyless custody holder
cannot verify row replay or prune operations itself.

## When pruning waits

`set_retention_policy(scope, policy)` names required peers, declared-behind
peers, copy count and retained window. Pruning releases only operations covered
by a verified, durable checkpoint, in atomic batches of 64, oldest first.
The newest `window` covered operations remain.

| `PruneBlock` | Cause |
| --- | --- |
| `NoCheckpoint` | No verified checkpoint |
| `NoPolicy` | No retention policy |
| `Unacknowledged` | A required peer not declared behind lacks the operation |
| `SoleCopy` | Fewer than `max(1, copies)` other nodes acknowledged it |
| `Custody` | Too few configured full nodes applied it |

Headers, bodies, acknowledgments and outbox entries go together. Declaring a
peer behind removes a required-peer wait, never the durable-copy requirement.
[Retention holds](retention-holds.md) and coordinated evidence keep their own
rules. Recovery export still needs complete mergeable history.

## Syncing a peer

A pruned prefix is a per-author sequence through which operations were
released. It is storage metadata, separate from seal finalization.
`pruned_page` returns it as a `PrunedPage`; `behind_pruned` compares it with
the receiving replica. Native requests use the checkpoint message family's
`Pruned` and `PrunedLogs`. Range pages keep their `behind` flag.

`NativeSession::pull_page` reports scopes needing rebootstrap instead of
receiving their frames; `pull` fails for such a scope. A frame at or below
the local pruned prefix returns `ReceiveStatus::Pruned` without storage.
Ordinary checkpoints retain tombstones; compaction at seals releases dead
rows, hidden losers, cascade orphans and their unused creation stubs.

## Rebootstrap

The SDK requests a checkpoint before ordinary catch-up. A graph-domain source
offers only a base at a seal on its followed path; a source without a graph may
sign a current checkpoint on demand. A scope with no offered seal base joins
by frames. The graph lists issuer-authorized checkpoint records with their
seal, signer, digest and rows; checkpoint bodies travel only on request.

The importer verifies pages and switches transactionally to the checkpoint.
A previously empty scope becomes readable as each page commits. Existing rows
stay readable until the switch. See [checkpoint import](checkpoint-import.md)
for staging, retained local operations and reports, and
[SDK readiness](sdk-bootstrapping.md#readiness) for progress.

A compacted seal that loses a race follows the winning path with writes
refused. The SDK imports a listed checkpoint there, else rebuilds from its
own frames or a peer retaining every frame, else waits for owner recovery.
Logged scopes whose old fold released records need a signed existence base
with the checkpoint. See [rebootstrap at seals](lazy-compaction.md#rebootstrap).

A replica refuses its own checkpoint signature. Without a graph an authorized
writer holding an imported base can sign a local replacement before offering
it back to its signer. A graph-domain holder serves its existing seal base to
other peers; it creates no arbitrary cut to change the signer.

Keyless custody can serve a complete sealed checkpoint archive at a seal.
Archive messages verify the signer's authority and page signatures; interrupted
archives retain a cursor, and incomplete ones offer no checkpoint. There is no
separate declaration exchange that automatically deposits archives.

## Retries

A pruned mutation handle is not authored again while retained: `author`
refuses it, and `author_buffered`/`lookup_mutation` return its operation hash.
Reusing it with other input conflicts. Once it falls outside the handle
window, its retry is a new operation.

## Verifying a file

Open replays the retained suffix over the verified base and compares it with
the stored rows. Every `audit(scope)` freshly checks migration accounts,
the scope's retained operations, global counters, handles, admission evidence,
all identity ledgers and NoFrames records before keyed row/base/frontier and
suffix checks. Unknown and keyless scopes retain their operation report;
zero rows/replay and an absent checkpoint do not assert keyed verification.

The full holdings fixture has a private `cfg(test)` composition over the same
production checks. One replica guard covers a finite nonempty, unique scope
list in input order. Complete global checks run before any scratch replay and
again after all scopes finish; every scope retains its own history and
projection checks. It exposes no public batch capability or validation cache.

Scratch replay keeps its existing page transactions and strict Check mode,
which never mints missing grants or observations. On ordinary Result exits,
verification restores temporary flags and attempts to clear only that scope's
scratch. A verification error takes precedence if cleanup also fails; cleanup
failure otherwise prevents success. This promises neither cancellation safety
nor one SQL snapshot or all-or-nothing batch rollback. Existing transaction
finalization may legitimately re-pin an older signed admission head to the
stored revision; normal current-head verification leaves authority unchanged.

## The signed statement

`SignedCheckpoint` contains a `CheckpointStatement`, the node signer and its
credential chain, signed under `rocky.encrypted-checkpoint.v3`. Its identity
is `rocky.encrypted-checkpoint.id.v3` over the bytes. Format **16** is the only
accepted checkpoint format; there are no legacy readers or migrations.

| Field | Binds |
| --- | --- |
| `seal` | The base's seal in a graph domain; absent for ordinary checkpoints without a graph |
| `tip`, `schema` | The sealed tip and schema generation; the graph resolves its manifest |
| `revocations`, `state`, `scope`, `partition` | Signer's revocation set, how the state is held (below), scope and optional authenticated tuple |
| `sequence`, `previous` | Position in the scope's checkpoint chain and prior identity |
| `frontier`, `clock`, `operations` | Per-author sequence/hash/clock, highest clock and covered count |
| `identities` | Per author, `sequence` over all covered births and `own` over that author's own births only (`own ≤ sequence`); deleted rows and consumed births included, carries excluded from `own` |
| `moves` | Count/digest of opaque move links in row records |
| `rows`, `tombstones` | Count/digest of stored row records and tombstone count |
| `creations`, `handles` | Creation witness and retained mutation-handle summaries |
| `custody` | Covered acknowledgments, sole copies and recovery archives |
| `coordinated` | The [coordinated-log binding](stuck-recovery.md#checkpoint-binding) |

`state` is `Keyed { key_epoch }` for rows sealed in pages under that epoch, or
`NoFrames`: the scope's state at the seal holds no frame history at all,
inherited history included. A `NoFrames` statement sits at a seal whose tip it
names, with no operations, frontier, clock, identities, rows, tombstones,
creations, moves or covered custody; it has no pages and no key epoch. See
[no-frames checkpoints](#no-frames-checkpoints).

A statement carries no admitted or finalized declaration fields. The current link
and replica file are `rocky.link.v14` and `rocky.encrypted-replica.v32`. Row frames remain format 11.

## No-frames checkpoints

A scope this node holds without its key cannot be read, yet a joiner whose
other scopes were pruned still has to verify it at a seal. When finalization
checked such a scope at seal X and this replica holds no history of its own
for it (no retained frame, void or placement visible at X, no applied, pruned
or logged positions, rows or identities, and no earlier non-empty base),
compaction signs a `NoFrames` base instead of skipping it. Covered keyless
ciphertext stays deferred and signs nothing. The signer needs the usual
checkpoint authority: whole-scope read plus a write right. Custody alone may
import and relay the original statement, never originate one.

Import stages a `NoFrames` record complete at once, with no pages, key,
counters or acknowledgments, and authorizes pruning nothing. Offer, begin
and finish check its shape, authority, seal and tip, and that no held
history of the scope is visible at the seal. Open and full audit check each
record in its own store: an installed base fully, with no rows, pages or
stubs and no contradicting history; a staged import or
[pulled state](kept-seals.md#pulled-states) for integrity only, since covered
history may legitimately arrive before it is used. Under an installed base, a
frame, void or placement of the scope that the seal includes is refused as a
contradiction inside the transaction that would store it; frames after the
seal follow the ordinary rules. The base supplies only its own scope's
verified prefix through X for later seals: it places no other scope.

### Exact coverage

Whether a recorded no-frames fact at seal X includes a frame is decided by one
operation, used for intake under an installed base, intake against a pulled
state, origination, install and open and audit: the frame's author position
must lie within X's own signed frontier, and its source tip must reach X's
tip along structural ancestry, inheriting each opening seal's frontier.
Which seal wins a tip today, or whether a revocation later excluded X, does
not change the answer, so a later graph change cannot turn retained evidence
beyond X into a contradiction. A frame whose binding cannot be decided counts
as covered. Live visibility for applying frames is a separate rule.

An import will also compare each incoming row's original creation
provenance with this replica's known claims before any destructive step; see
[admission evidence](admission-evidence.md#checkpoint-comparisons)
(implementation pending).

## Design rationale

Checkpoints are per scope because keys, read authority and custody are per
scope. Set digests use LtHash16 with 1,024 lanes and SHA-256 of the accumulator,
maintained transactionally so an ordinary checkpoint need not scan live rows.

The target-scoped [SHA-2 backend](../features_09/17-sha2-backend-evidence.md)
uses the upstream hardware dispatcher on supported little-endian AArch64
families. Fixed pre-change vectors cover complete lanes, Sets, checkpoint
signing bytes, signatures and IDs; labels, canonical encodings and formats
are unchanged. Backend selection does not replace replay or authority checks.

## Limits

- Surviving rows are stored twice, in the live workspace and its replay base.
  A creation stub remains for each stored row and required suffix dependency.
- A checkpoint names at most 256 logs and fits one 256 KiB frame.
- Pages hold at most 256 rows and 63 KiB plaintext, leaving room for framing
  and author bindings; every page carries its signer's signature.
- A missing pruned dependency other than a predecessor or creation witness
  waits for evidence or a replacement checkpoint.
- The final switch and projection rebuild are one transaction proportional
  to the scope's rows. Constraint candidates and support rows are derived,
  with [partition-local rebuilds](partitioned-scopes.md).
- A row's author bindings are stored by digest; see
  [stored records](checkpoint-import.md#stored-row-records).
- Recovery export and recovery custodians still require complete mergeable
  history. The ordinary pruning copy gate never releases the last copy.
