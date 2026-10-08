# 7. Strong state, commands and external effects

> Superseded implementation brief, retained as an optional Room acceptance
> reference. Do not implement this sequence as the current Rocky roadmap.
> Generic work and deferred adapter work are assigned in
> [features_04](../../features_04/README.md#foundations-and-old-plan-disposition).
> [V4](../../proposals/architecture-proposal-v4.md) governs future design.

Status: superseded future brief; retained optional Room requirements. Owner: installed applications/command handlers, runtime strong
execution and SDK access modes. Coordinator remains generic. Depends on phases 3–6.

## Preserve distinct guarantees

Ordinary mergeable DAO transactions commit local deterministic row operations.
They do not create globally serializable database transactions. The existing
strong path seals complete invariant-relevant inputs, checks predecessor/fencing,
prepares immutable results and obtains coordinator confirmation outside database
transactions. Confirmed state is separate from mergeable registers.

Generate explicit read/write modes for each logical table/view: mergeable local,
confirmed read-only, declared overlay or registered command target. A join between
mergeable and confirmed views must have a documented freshness/cut meaning;
it cannot accidentally claim a common confirmed snapshot. Default shopping CRUD
uses the mergeable mode and needs no strong API in its repository.

Confirmed SQL views expose only currently authorized, available projections with
the expected schema/handler generation and receipt evidence. Missing payload,
key, required predecessor or coordinator completeness remains unavailable.
Learning a head without its payload cannot produce invented empty state.
Strong-read modes acquire metadata/payload before starting their local snapshot;
coordinator and network calls remain outside storage transactions.

## SQL and business invariants

Local NOT NULL/type checks can remain SQL constraints. Global secondary uniqueness,
inventory reservations, account balances, ACL/ownership transitions and invariant
foreign-key workflows need registered deterministic strong semantics. Installing
a normal SQLite constraint does not supply distributed enforcement.

For transparent DAO access to command-controlled tables, research generated
bindings from declared statement/transaction forms to registered handlers and
typed arguments. The handler must be deterministic, permission-checked and bound
to a complete invariant domain; arbitrary SQL or Kotlin transaction closures are
not executable remote handlers. Unknown write shapes fail before mutation rather
than falling back to mergeable writes.

A local SQL transaction cannot simply remain open while waiting for confirmation.
A viable command binding closes its read/preparation transaction, persists the
immutable invocation and exact expected input cut, performs coordination, then
atomically applies the verified result. Define provisional read/row-count/ID
behavior and stale-input rejection. Separate commands on independent coordinator
heads do not implement one atomic cross-domain command.

This transparent strong-write binding is an unresolved design gate. If the
required application expects arbitrary ordinary `@Transaction` bodies to become
strong distributed transitions without declarations, stop and resolve that
architecture; do not add Rocky invocations to its repository as a workaround.
A mergeable-only Room profile must state that limitation at configuration.

## Uncertain outcomes

Expose queued/prepared/outcome-unknown/confirmed/applied/custody progress at the
database lifecycle boundary, with stable native identities. Coordinator timeout
after confirmation cannot become successful unconfirmed CRUD or a definite
rollback error. Recover through receipt lookup and retained preparation without
running a deterministic transition twice against a different predecessor.

Fenced executors, revoked callers, incomplete state and handler changes retain
existing rejection rules. Coordinator receipts bind results but confer no row
read/write grant. Package filtering and subset verification preserve private
companions and do not prove complete invariant replay.

## Effects, jobs and blobs

SQL triggers inside a transaction must be local deterministic state transitions.
They never send email, HTTP or coordinator calls. Strong handlers emit scoped job,
result/blob/checkpoint intents. Job discovery/read views may be generated as
read-only SQL, while dispatch stays in application lifecycle/workers.

Preserve durable signed effect-start/outcome observations, stable provider
idempotency keys, uncertain-send handling and executor fencing. Replaying SQL,
rebuilding a projection, importing a backup or reopening Room never repeats an
external call. Blob/result references retain payload availability and scope
checks; absence of encrypted bytes is not a nullable entity value by default.

Cross-app downstream commands retain independent authorization and head checks.
An Alice-like local intent cannot claim an atomic Accounting update. Retries,
compensation and human resolution are workflow state, not SQL rollback.

## Completion gate

- Existing strong commands, exact-cut reads, takeover and interrupted-confirmation
  tests pass through the new schema/session projections.
- Ordinary mergeable SQL cannot modify permission-controlling/confirmed state,
  invoke a handler by writing a hidden table, or gain coordinator rights.
- A declared command binding proves its DAO/transaction behavior end to end, or
  remains blocked; no generic strong-write compatibility claim is made.
- Mixed private/public outputs, missing payloads, late predecessors, policy renewal
  and recovery preserve scoped visibility and original confirmed bytes.
- Reopen/migration/rebuild never dispatch effects; retries preserve provider keys
  and uncertainty. Independent domains never masquerade as one confirmed commit.

Run command/coordinator/effect regression suites and `./scripts/check`.
See [encrypted commands](../../runtime/encrypted-commands.md),
[strong publication](../../runtime/strong-publication.md) and
[recovery/effects](../../runtime/recovery-and-effects.md).
