# Current implementation and developer API audit

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: historical source review of MVP parent `8dd33dd0`, 2026-10-03. Findings describe
the reviewed MVP code or a specific limitation of its API.
Phase 1 resolves these findings; see the evidence below. All IDs are addressed by
[phase 1](01-audit-fixes.md); larger V3 capabilities follow its completion.
Source links name the owning definitions so this review remains navigable as
line numbers change. F01/F03 have focused executable probes; other findings are
source-confirmed risks/contracts, not claims of reproduced data loss.

## Phase-1 resolution evidence

Implemented on 2026-10-03. The [supported API](../../runtime/supported-api.md) and
[recovery/compatibility guide](../../runtime/audit-recovery.md) describe the resulting
contracts. Original findings below remain as the audit record.

| Finding | Implementation and regression evidence |
| --- | --- |
| F01 | [Receipt isolation](../../../crates/rocky-runtime/src/worker/strong.rs), retained rejection evidence and preserved preparations; `rejection_isolated_from_independent_work_and_preparation_survives`, existing forgery/outage tests |
| F02 | [Offline strong restoration](../../../crates/rocky-runtime/src/worker/restore_strong.rs); independent corruption suite for metadata, receipt, hash key, flags, counter, handles and outbox; existing missing-package/old-epoch/sealed-cut tests |
| F03 | Explicit author bindings/targets at receive and reopen; `custom_scope_and_client_only_invoker_persist_and_reopen`, `three_named_peers_have_independent_operation_and_result_obligations` |
| F04 | [Installed descriptors](../../../crates/rocky-core/src/storage.rs), pre-write comparison and WAL transactional initialization; `interrupted_initialization_rolls_back_and_definition_mismatch_writes_no_schema` |
| F05 | Optional registered `CommandHandler`; [compiling local-only ledger example](../../../crates/rocky-runtime/examples/ledger.rs) and its no-command regression |
| F06 | Before/after admission with policy context and whole-row selection; ledger mixed value/owner patch denial at local/incoming boundaries; paired Todo row/field grants test |
| F07 | Scoped row snapshots and local-head evidence separated from publication history; named-peer status; existing observation registration/cancellation/coalescing suite |
| F08 | Queue byte permits, frame/history/observer quotas, continuation pages and yielding worker passes; bounded page/no-duplicate, quota and queued-query fairness regressions |
| F09 | [File lifecycle](../../../crates/rocky-storage/src/database.rs) separated from scoped domain scheduling; scope-bound author/revision keys; documented and tested exclusive single-domain mode |
| F10 | Reusable mutation/command handles, lookup, preconditions, structured reasons and preserved error sources; cancelled-waiter/reopen/idempotent/conflict tests and existing atomic rollback tests |
| F11 | Async coordinator futures and scope-bound heads/caller confirmations; distinct invoker/executor grants; client-only test and single-thread async coordinator barrier |
| F12 | Per-transition projection statements retained in deterministic order; two-table ledger batch/reopen equivalence and injected whole-batch rollback |

Verification: `./scripts/check` passes formatting, workspace Clippy with warnings
as errors, 63 tests and doc tests. `cargo run -p rocky-runtime --example ledger
--locked` passes. Native macOS `native_smoke` initial/reopen scenarios and
`native_publication` pass using fresh `local/audit-fixes-*-smoke-20261003-final` files.
The native runs exercise offline edit/convergence and immutable publication,
coordination outage recovery, peer application and Phone takeover. This does not
qualify networking, production authentication, Raft, physical power-loss durability
or same-file concurrent domains.

Compatibility: storage format 2 rejects MVP files before writes. Confirmation/
receipt bytes now bind caller principal; request/package hash vectors remain
unchanged. API breaks and the explicit file rejection/recovery path are recorded
in [audit recovery](../../runtime/audit-recovery.md#protocolapi-compatibility).

## What is worth preserving

The crate dependency boundaries match V3: core is engine-independent, applications
own reducers, runtime owns transactions, and coordinator metadata excludes app
payloads. Canonical bytes, checked counters, dependency readiness, permanent todo
tombstones and original-byte identity checks have meaningful tests. Projection,
operation, outbox and revision commit together. Observation registers before its
first snapshot and coalesces committed revisions. Durable preparation, lookup
after lost replies, exact-cut replay and executor fencing are useful foundations.

The existing suite validates the trusted demo. It does not establish an
application framework, authenticated transport or durable coordination service.

## Correctness and recovery

### F01 — Invalid strong results persistently block valid work (high)

[`receive_result` / `reconcile_strong`](../../../crates/rocky-runtime/src/strong.rs)
persist a package and its structurally matching receipt before external
verification. Every reconciliation scans all receipts and returns immediately on
`InvalidCommitment`. There is no rejected/quarantined verification state.

**Probe:** submit an unconfirmed package with a forged, matching receipt and a
wrong epoch, then enqueue another legitimate snapshot. Receipt acceptance fails,
but `execute_pending` fails on the same retained receipt before reaching the valid
request. The durable record also survives reopen. The existing forgery test later
confirms the same package, so it does not cover this obstruction.

Fix per-result rejection isolation and preserve retryable verification outages.
Keep the evidence without letting an invalid record block independent requests;
a genuinely missing confirmed predecessor must still block its strong chain.

### F02 — Reopen does not validate the strong ledger as a chain (high)

[`restore`](../../../crates/rocky-runtime/src/worker/incoming.rs) checks ordinary
history/projections, but counts strong publications through
[`publications` / `attempts`](../../../crates/rocky-runtime/src/worker/strong.rs).
Those functions decode bytes and trust stored `applied`/`verified` flags.
They do not establish that every applied package has matching receipt evidence,
exact replay, a contiguous predecessor chain, or a head matching strong metadata.
`command_sequence`, preparation hash keys and strong outbox/ack consistency also
lack the ordinary ledger's restoration checks.

A damaged applied package/head can therefore reach queries or stall preparation
without a deliberate restore error. Validate local trusted verification evidence
and chain/counters/outboxes on open. This checks accidental corruption; it cannot
make an attacker-controlled database trustworthy. Preserve offline reopen of
previously verified receipts without requiring a live coordinator.

### F03 — Accepted custom identities cannot reliably reopen (high)

[`Replica::open`](../../../crates/rocky-runtime/src/lib.rs) accepts an arbitrary
`Identity`, and application policy may authorize arbitrary nodes. Runtime
[`receive`, `restore`, `validate_peer`](../../../crates/rocky-runtime/src/worker/incoming.rs)
nonetheless hardcode Laptop/Phone. Strong status and acknowledgment paths
[choose the opposite fixture](../../../crates/rocky-runtime/src/worker/strong.rs).

**Probe:** authorize node `[9; 16]`, create a todo, close, reopen using identical
identity/policy. The write succeeds; reopen returns `Corrupt`. Move peer admission
into explicit runtime configuration/authenticated evidence and keep fixtures in
demo composition. Exercise three peers and unrelated caller identities.

### F04 — Persisted compatibility is not bound to the installed app (high)

[`identity_bytes` and `mutate`](../../../crates/rocky-runtime/src/worker.rs) hardcode
four version values and `Versions::V1`; `StoredApplication` supplies no stable
installed definition/projection fingerprint. Two implementations can claim the
same identity while changing schema, decoding or reducer behavior. Existing-file
open checks identity bytes, not that app contract, and additive strong/peer DDL
runs before full restoration validation.

Persist an explicit application compatibility descriptor and storage version;
reject unsupported definitions before schema writes. Define atomic migrations
and distinguish protocol, schema, projection and handler versions. Retain original
wire bytes. Phase 1 needs correct binding/rejection, not a complete upgrade system.

## Application-author and consumer API

### F05 — Every stored application must implement a snapshot command (high)

[`StoredApplication`](../../../crates/rocky-core/src/storage.rs) requires
`snapshot_method`, `snapshot_bytes`, `admit_command`; generic
[`publish_snapshot`](../../../crates/rocky-runtime/src/strong.rs) exposes this todo
workflow to every application. [`StrongRequest` / `StrongPackage`](../../../crates/rocky-core/src/strong.rs)
have no argument or general transition/effect representation. Application authors
cannot express stock reservation using this contract, or implement a local-only
app without supplying unused command behavior.

Separate optional strong handlers from projection/reducer contracts. Keep
PublishListSnapshot in todo's supported application interface. Phase 1 removes the
mandatory coupling and establishes a real extension boundary; phase 2 implements
and qualifies the general command/result model.

### F06 — Admission cannot implement V3 before/after row policy (high)

[`Application::admit`](../../../crates/rocky-core/src/lib.rs) receives only envelope
and policy; `apply_ready` receives state but no caller authorization context.
`admit_read` is one boolean decision for a whole scope. Todo's
[`StaticPolicy`](../../../examples/todo-app/src/policy.rs) has global action/field flags,
not paired row/field grants or resulting-row checks. There is no explicit policy
cut for pending operations versus later revocation.

Specify an admission context and candidate transition validation that works at
local, incoming and command boundaries. Define replay of historically admitted
operations separately from current authorization. Keep static policy explicitly
trusted until phase 3 supplies signed grants and encrypted row enforcement.

### F07 — Query and progress surfaces cannot express their evidence (medium)

[`query` / `observe` / `Snapshot`](../../../crates/rocky-runtime/src/lib.rs) expose
one compiled row list and every applied publication. There are no bindings,
query options, query scope, completeness or coordinator-bound read mode.
`StrongStatus` aggregates one implicit peer; its enum mixes local command lifecycle
with peer application. A consumer must poll unrelated APIs and copy all retained
publication packages to render a list.

Separate query data, local revision, strong-head freshness and named-peer progress.
Define cancellable tracking/coalescing. Explicitly document local-read staleness;
reserve a strong-read contract for phase 2. Do not expose raw unrestricted SQL as
an interim fix for row permissions.

### F08 — Bounded pages hide unbounded work and retained state (high)

[`SyncState`, `missing_operations`](../../../crates/rocky-runtime/src/replication.rs)
exchange the entire known-ID set; the page limit bounds only returned records.
[`process_pending`](../../../crates/rocky-runtime/src/worker/incoming.rs) loops through
all ready batches within one worker command. Restore and replay retain all history;
[`status`, `attempts`, `strong_results`](../../../crates/rocky-runtime/src/worker/strong.rs)
materialize all requests/packages repeatedly, and every query includes publication
history. Fixed queue item counts do not impose byte/history/observer budgets.

Add work/byte quotas, continuation pages and fair yielding; reject oversize work
before persistence/allocation where possible. Document full-history limits until
checkpoint/retention support exists. Never prune unresolved or sole recovery data
merely to meet a quota.

### F09 — Runtime architecture cannot exercise the selected MVCC workload (high)

[`Worker`](../../../crates/rocky-runtime/src/worker.rs) owns one scope, connection,
author counter and revision; its queue serializes queries and all writes. An
exclusive file lock prevents composing multiple workers for one file. Separate
replica files do not demonstrate V3 independent writers sharing a database.

Phase 1 must separate file ownership from domain scheduling and document the
current serial contract; phase 2 qualifies multiple domains/connections and scoped
revisions. Preserve serialization of conflicting domain/author/crypto state and
whole-transaction retries. Do not assert an MVCC speedup from current demo tests.

### F10 — Callers lose actionable errors and durable handles (medium)

[`Error` and `application`](../../../crates/rocky-storage/src/storage.rs) stringify
application errors and expose Turso errors through the public SDK. `StrongPending`
is a string for different recovery actions. `MutationReceipt` yields an envelope
only if its in-memory waiter survives; no stable handle supports later tracking,
preconditions or recovering a submitted mutation's outcome after waiter loss.

Expose stable structured failure categories, preserve error sources, and provide
accepted operation/request handles with evidence-based tracking. Specify enqueue,
cancellation, timeout and close semantics. Preserve optional waiting for locally
admitted work; distinguish durable acceptance from merely entering the queue.

### F11 — Coordinator port blocks async callers and conflates roles (medium)

[`Coordinator`](../../../crates/rocky-core/src/strong.rs) has synchronous I/O-shaped
methods called directly inside runtime async methods. A network implementation
would block the caller's executor unless it adds its own bridge. There are no
deadline/unknown-outcome semantics in the port.
[`confirm`](../../../crates/rocky-coordinator/src/lib.rs) requires the request origin
node to be in the eligible-executor list, preventing a separately authorized
client-only node from issuing a command. `StrongHead` omits scope, so the returned
head's domain is implicit in a trusted call.

Use an async coordination port with explicit scope bindings and cancellation/
unknown-outcome behavior. Separate invoke, execute and takeover authorization;
keep coordinator checks generic. Signed network evidence and Raft follow later.

### F12 — Projection batching has an unstated adapter constraint (medium)

[`process_pending`](../../../crates/rocky-runtime/src/worker/incoming.rs) reduces a
batch into its final state, then calls `project(final_state, operation)` for each
operation. Local mutation calls `project` with that operation's immediate next
state. [`StoredApplication::project`](../../../crates/rocky-core/src/storage.rs) does
not explain this distinction or prohibit incremental/state-sensitive projection
logic. Todo's idempotent entity UPSERT works; a different adapter may diverge
between local application and incoming batches.

Choose and document one contract: per-transition projection changes, or an
explicit affected-entity projection from final state. Test a second app with
multiple related operations in one incoming batch and verify query/reopen/replay
agreement and atomic rollback.
