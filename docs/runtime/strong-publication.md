# Strong snapshot publication

Strong commands on the trusted `Replica`, demonstrated by the todo app's
`PublishListSnapshot` with in-memory coordination. The
[strong-command contract](../protocol/strong-command-contract.md) defines the
invariants and [encoding vectors](../protocol/strong-encoding.md) the bytes.

## Run and use the desktop demo

```sh
cargo run -p todo-desktop --locked -- --data-dir local/todo-demo
```

Publish snapshot on either panel seals that replica's applied frontier and visible
rows, then durably queues a command. Laptop initially executes commands, including
Phone's requests. Missing input stays pending until ordinary operation sync supplies
it. The executor rebuilds the original cut from the checkpoint and the operations
after it, even if its live list already includes later edits. An unresolved
request holds the [checkpoint](checkpoints.md#when-pruning-waits) at its cut
until it resolves or is [released](retention-holds.md#an-unresolved-strong-request). Numbered publications appear through committed
observations only; they retain their original rows after edits and deletion.

Disable coordination to leave publications pending while editing and operation
replication continue. Enable it to reconcile outcomes before another preparation.
The status distinguishes queued, prepared, outcome unknown, mock confirmed locally
and named-peer applied, plus terminal retired/expired identities under
[bounded coordinator retention](coordinator-retention.md). Peer received is reported separately from peer applied.
A mock confirmation alone does not establish that Phone has applied a publication.

Take over to Phone is an explicit owner control. It checks Phone's confirmed head
before and after fencing Laptop. Every command preparation checks it again. This
is a static development grant, with no election or failure detector.

## Runtime and application ports

`rocky_core::Coordinator` supplies async trusted metadata calls: `head`,
`confirm`, `lookup` and `verify`. `rocky_coordinator::MemoryCoordinator` implements
the port with a mutex-serialized state machine. Composition registers the fixed
scope and eligible executors, acquires Laptop at epoch 1, and owns availability
and takeover controls. Confirmation carries identities, generations and
commitments; the coordinator receives no todo rows, DB handles or reducer.

`StoredApplication` optionally registers `CommandHandler` methods. Todo composition
selects `todo_app::PUBLISH_LIST_SNAPSHOT` through `Replica::request_command`.
Handlers define pure validation, sealing and exact-cut execution plus current
admission. Merge-only apps require no command methods. See
[the supported API](supported-api.md) for contracts, handles and limits.

`request_command` allocates a persisted command counter, separate from operation
sequences, and persists a `StrongRequest` before returning it.
`receive_request` admits canonical retries without replacing their bytes.
`execute_pending` serializes executor passes, looks up each unresolved request
before preparing/submitting, and checks the current confirmed head. A prepared
attempt is recorded as outcome unknown in the commit that stores it. Coordination
calls execute in the caller, outside the storage worker and all DB transactions.
A slow coordinator therefore does not hold a storage transaction or stop edits.

`strong_status` exposes command progress, local confirmed head, durable result
receipt/application state, and named-peer acknowledgments. `strong_results`
returns retained package/receipt records for reconciliation, including received
results still awaiting validation/application; use `confirmed_publications()` for final
results. `receive_result` asks the coordinator about the receipt, then stores
the canonical package/receipt bytes, records the verdict and applies what is
ready in one commit; a receipt the coordinator could not be asked about is stored
unverified (see [design rationale](#design-rationale)).
`reconcile_strong` validates receipts against the accepted
mock ledger and resumes dependency/predecessor-ready application. Already validated
receipts can apply during an outage; unvalidated receipts cannot.

`Snapshot` supplies scoped live rows, applied frontier, revision and local
strong-head evidence. Publication history is queried explicitly. Publication application atomically advances the strong head,
marks the package applied, creates its durable outbox record and advances the same
local observer revision. Wakeups follow commit. Strong sequences and mergeable
frontiers remain separate.

## Storage and replication

Additive tables retain command counters/configuration/head, requests, immutable
preparation attempts, receipts and verification/application flags, publication
outbox work and named-peer acknowledgments. The immutable package itself is the
publication projection; no live todo rows are replaced. Requests/preparations are
kept for this bounded demonstration, including attempts rejected by epoch/head
fencing. A timeout preserves the attempt as outcome unknown.

The existing bounded simulated transport has explicit `StrongRequest`,
`StrongResult` and `StrongAcknowledgment` messages. Results carry canonical package
and receipt bytes. Reconciliation transfers missing requests/results and repairs
lost receipt/application acknowledgments. Ordinary retained-operation transfer
supplies missing input; retained result history supplies predecessor packages.
Reordered results wait behind the predecessor chain. Duplicate delivery does not
advance revisions or allocate another publication sequence. Applied acknowledgments from every configured target clear outgoing work while
retaining package history.

Rejected receipts retain evidence without erasing preparations or blocking
independent work. Reopen validates the strong ledger offline; see
[audit recovery](local-replica.md#audit-and-offline-verification).

## Recovery and limits

After an app-node interruption, reopen the same DB with the same identity and keep
the coordinator instance alive. Look up the request outcome before submitting any
new attempt. An established receipt requires its exact committed package. Missing
the only package copy blocks strong progress; the head is never rewound.

A full desktop restart creates a new coordinator configuration and loses its
accepted ledger. A DB with persisted coordination configuration reports
`CoordinationStateLost` and blocks strong work. Existing publications remain
queryable, and ordinary edits/sync continue. Use an explicit fresh data directory
for a fresh demo; no automatic reset or client-derived authority is implemented.

Receipts are attested by an in-memory fixture ledger. There are no signatures,
quorum verification, durable Raft service, credentials, encryption, production
retention or coordinator disaster recovery. App-node reopen tests and exception
fault injection do not establish physical power-loss durability.

## Verification

```sh
cargo test -p rocky-runtime --locked
cargo run -p todo-desktop --example native_smoke --locked -- local/strong-publication-smoke publication
```

Tests cover canonical SHA-256 vectors, concurrent expected-head proposals,
admission, fencing and historical retry, dependency closure, forged receipts,
outage verification, duplicate/reordered results, takeover, ledger/package loss
and every interrupted request/preparation/accept/apply boundary
(`preparation_acceptance_and_apply_failures_recover_at_each_boundary`,
`death_between_the_merged_steps_of_a_result_leaves_none_of_them`,
`losing_buffered_strong_steps_keeps_the_exposed_attempt_and_recovers`).
`strong_cut_tests` cover a cut bounded by the executor's checkpoint base on a
domain with more than 100,000 lifetime operations (a soak test in the e2e
tier, `./scripts/check --e2e`), verification after
rebootstrap and after pruning, refused bases, and a rejected operation inside a
cut ([Decision 0042](../decisions/0042-bounded-strong-cut.md)). The
smoke run drives the WebView through publication, outage, resumption at the
original cut and takeover.

## Design rationale

A command takes three commits on the executor; each boundary is where a crash
is distinguishable after recovery:

| Commit | Why it stays separate |
| --- | --- |
| Publish | A separate call, often on another node; the sealed request leaves the device |
| Prepare (binds the head, records the attempt `unknown`) | The package must be on the device before the coordinator is asked: an established receipt needs its exact package |
| Result (stores package and receipt, records the verdict, applies) | Follows the coordinator call; buffered durability flushes it only when exposed |

- A received result is stored only after the coordinator call returns. An
  unreachable coordinator still yields `Unavailable` and an unverified stored
  receipt, so offline devices keep what they exchange; a process that dies
  mid-call stores nothing and the sender offers the result again.
- `reconcile_strong` uses an internal non-exposing read, so reconciling does
  not flush a buffered result. Public `strong_status`/`strong_results` expose.
- An attempt is `unknown` from its own commit, so `release_command` refuses
  it and `RetentionPolicy::request_hold` keeps holding checkpoints until
  `execute_pending` has looked the outcome up. Holding too long is accepted;
  too short is not.
- Known gap: when `confirm` answers `StaleHead` or `StaleEpoch`, the attempt
  stays `unknown` and `execute_pending` returns `StrongPending(StaleFence)`.
  The request is unreleasable until a later attempt applies.
