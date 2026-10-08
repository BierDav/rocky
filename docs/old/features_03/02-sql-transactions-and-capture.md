# 2. SQL transactions and precise capture

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented on the trusted runtime as a Rust API; see the
[guide](../../runtime/sql-sessions.md) and [decision 0016](../../decisions/0016-sql-transactions-and-capture.md).
Peers apply a transaction's operations one at a time, and the scheduling
cost of a lease is counted but not measured under the gate workload.
Owner: `rocky-runtime` writer/SQLite boundary, `rocky-core` transaction
evidence. Depends on [schema and identity](01-logical-schema-and-identity.md).

## Required boundary

Introduce a runtime-owned SQL session with connection, statement, cursor and
transaction handles. Its state machine is idle → active → committing → committed,
or active → rolled back; uncertain commit outcomes are recoverable by a durable
transaction token. Preparation/binding is not mutation admission. Only the runtime
can create protocol records or complete the physical database transaction.

Execute ordinary SQL on typed projections within a leased native transaction.
Reads on that connection see its pending writes; other connections see committed
snapshots. Implement autocommit statements, DEFERRED/IMMEDIATE/EXCLUSIVE modes,
COMMIT/END, ROLLBACK and nested SAVEPOINT/ROLLBACK TO/RELEASE behavior. Preserve
SQLite's statement-level conflict semantics instead of treating every error as
an automatic whole-transaction rollback. Security/capture failures poison the
transaction and must prevent its commit.

Serialize the lease through the existing per-file writer, not an independent
foreign-owned writer connection. A lease cannot outlive its session without
defined cleanup. Avoid holding a Rust mutex across foreign execution; the worker
owns connection state between calls. Lease limits, cancellation and statement
budgets bound interference with incoming sync and other domains. Document a
transaction timeout as a real compatibility limit, not an invisible truncation.

## Capture and commit

Recommended first implementation: extend the existing safe preupdate callback
with a transaction capture sink. One hook fans out to digest tracking and exact
old/new row capture. Callback failures are latched and checked before SQL success
or commit; no panic or silent loss. It must cover trigger/cascade writes, ordinary
rowid and WITHOUT ROWID tables, and replacement/key changes. Protocol tables and
derived writes have installed classifications rather than name-prefix guesses.

Capture has nested rollback accounting. Reconcile the surviving events with the
actual final SQL rows before sealing a canonical intent. Savepoint rollback must
remove captured effects and restore digest state. Choose explicitly how temporary
insert/delete and delete/reinsert transitions produce incarnation evidence.
The [session extension](research.md#sqlite-mechanisms-evaluated) is an alternative
capture tool, not an assumed ready-made transaction protocol.

Replicate precise row values/identities, never re-execute the original SQL on a
peer. Predicates run once against the author's authorized transaction snapshot.
Time/random/default expressions become recorded inputs; after returning them to
the application they cannot be silently regenerated during a retry. An SQL
`count = count + 1` in a register table publishes the computed value, not a CRDT
increment; counter/set semantics require an installed operation/command policy.
State that difference at schema configuration rather than guessing user intent.

At commit:

1. Freeze the surviving changed rows, assigned-field masks, incarnations and
   transaction preconditions. Read exact before images from the transaction's
   initial state and candidate after images from its final state.
2. Validate schema, deterministic app rules and every generated effect under
   current session authority. Phase 4 specifies signed/scoped admission.
3. Allocate stable operation/bundle identities and persist the immutable intent,
   canonical operation bytes, projection/digest, outbox, counters and revision
   together in the same file transaction. Reconcile provisional SQL state with
   the reducer's authoritative projection; they must represent the same commit.
4. Complete SQLite COMMIT and the configured durable flush, then publish results,
   observer invalidation and outgoing data. Default foreign commit acknowledgment
   is flushed, matching the current desktop SDK's acknowledged-write contract.

Precommit SQL row counts, `last_insert_rowid()` and RETURNING belong to the
application connection. Internal protocol inserts must not overwrite those
observable values. Returned rows/IDs inside an explicit transaction are provisional
until commit. In autocommit, do not report statement completion before its commit
outcome is known. See [SQLite RETURNING](https://www.sqlite.org/lang_returning.html).

## Update policies and exact retries

Changed-values mode compares canonical before/final values and publishes only
differences. It does not move a register stamp for an unchanged value. A no-op
write retains native SQL row-count semantics even when no operation is emitted.

All-assigned-values mode republishes fields in the statement's assignment mask,
including equal values; an entity `@Update` may assign every mapped field. This
needs a protocol write mask: current `RowChange.changed_fields()` cannot represent
an equal-value write. Grant checks cover every republished field. Generated side
effects need the same explicit rule. Do not silently convert field-level SQL
updates into whole-row writes.

Persist a transaction token before accepting a commit intent. Recovery returns
the original outcome and bytes even after later edits. A token with different
input conflicts. Network transfer and foreign cancellation never regenerate a
committed operation. Keep enough outcome evidence after pruning to recover an
interrupted acknowledgment.

SQLite contention retries may rerun native deterministic work before visible
results; they must not replay arbitrary Kotlin transaction bodies. A busy-snapshot
error after application reads is returned, not secretly recomputed against a
new snapshot. Define retryable/definite/uncertain errors separately.

## Completion gate

- Multi-field/entity statements, bulk inserts and multi-statement transactions
  commit atomically, with read-your-writes and no external dirty reads.
- Nested rollback, constraint algorithms, triggers/cascades, no-ops, key changes,
  generated IDs and RETURNING match SQLite's documented behavior.
- Faults during capture, signing, projection, protocol insertion, COMMIT and flush
  never publish a partial commit; restart/lost acknowledgment recovers once.
- Reordered duplicate delivery converges to the locally committed reducer state;
  capture and internal SQL never feed back as new user writes.
- Capture/digest rollback passes existing checkpoint tests; long foreign leases
  have measured, bounded scheduling impact.

Run runtime/SDK regression and deterministic subprocess crash tests plus
`./scripts/check`. Power-loss qualification remains separate from process exits.
