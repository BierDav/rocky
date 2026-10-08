# Phase 2: persistent local replica and UI

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented. See the [local replica guide](../../runtime/local-replica.md) for APIs,
engine qualification and verification. The requirements below remain the phase
acceptance contract. It was built on Turso 0.8.1; the engine is now SQLite
([decision 0007](../../decisions/0007-sqlite-engine.md)).

## Goal and required reading

Create/edit/complete/delete a todo through Rocky, commit it to disk, update the UI
from a committed observation and reopen the same data successfully.

Read [MVP scope](README.md), [root instructions](../../../AGENTS.md),
[module boundaries](../../modules.md), [phase 1](01-operation-semantics.md) and the
[operation contract](../../protocol/operation-contract.md). Inspect phase 1 code/tests first.

Storage references: [engine decision](../../../benchmarks/engine-comparison/DECISION.md),
[v3 engine boundaries](../../proposals/architecture-proposal-v3.md#10-engine-recommendation-and-evidence),
[transaction ownership](../../proposals/architecture-proposal-v3.md#11-runtime-ownership-and-transaction-boundaries)
and [observation](../../proposals/architecture-proposal-v3.md#13-sdks-sql-reads-and-observation).

## Ownership and engine qualification

- `rocky-runtime` owns the DB, transaction completion, operations, outbox and
  observers. `todo-app` owns schema definitions, reducer and projection meaning.
- `todo-desktop` composes the runtime/app and supplies Dioxus components and main.
  It must not run direct SQL writes or maintain a second authoritative todo list.
- Preserve runtime -> core and app -> core. A small shared application contract
  can supply schema statements, decoded inputs and deterministic projection
  changes; the runtime executes controlled changes in its owned transaction.
  Use bound SQL values and restrict this internal adapter to the installed app.
- Begin with the benchmark's exact Turso 0.8.1 dependency as the qualified reference.
  It is the Rust engine, not libSQL or a hosted service. A version change requires
  documenting and rechecking the affected assumptions, not copying the benchmark.
- Inspect [benchmark initialization](../../../benchmarks/engine-comparison/src/main.rs)
  around `Db::open`, `Db::conn` and `PRAGMA journal_mode`; it is evidence, not a
  production adapter. Qualify MVCC initialization, `BEGIN CONCURRENT`, binding,
  commit/rollback and error classification against the installed crate API.
- Configure and verify the intended FULL/fullfsync durability settings on macOS.
  Fail with a useful error if required settings/transaction APIs are unavailable.
  Do not silently substitute an in-memory store or another engine.

Serialize mutation work for this single-author/domain prototype. This does not
establish independent concurrent-domain performance. Do not enable full CDC.

## Storage and mutation contract

Use one file under `--data-dir` for the Laptop replica. Persist and verify its
fixture node/app/domain identity and schema version before opening for writes.
Opening a file with incompatible identity/version fails without deleting data.

Provide framework-owned tables for original operation bytes, merge/processing
metadata, author sequence/clock, per-domain revision and outgoing work. The todo
app supplies its projection schema. Preserve canonical history and tombstones.

A local mutation performs the following under the author write boundary:

1. Validate current scope, policy, action and existing/resulting row as applicable.
2. Allocate the stable operation identity, dependencies and next Lamport time.
3. Encode the operation and reduce it using the installed app contract.
4. In one transaction, persist operation, counters, metadata, projection, outbox
   and revision. Commit before reporting local-durable success or notifying UI.

Retry a retryable conflict by retrying the whole transaction with stable operation
identity/bytes under the same author boundary. Use bounded retries and backpressure;
otherwise return a typed failure. Rollback must not leave partial protocol state.
No transaction or read snapshot remains open while waiting for UI/external work.

Remote processing is phase 3. Keep origin metadata now so remote materialization
can later avoid authoring operations or producing local outgoing work again.
No delivery occurs yet; the outbox still exists and survives reopen.

## Query and observation contract

Expose query snapshots as rows plus local domain revision. The observer emits an
initial snapshot and later committed snapshots; queries alone do not subscribe.

Use a bounded latest-revision/watch mechanism and requery after invalidation.
Prevent the registration/initial-read race: register before reading and reconcile
the observed revision afterward, or use an equivalent proved atomic protocol.
Rows and revision must come from the same consistent, short read snapshot.
Close it before delivering to a slow consumer. Coalesce updates; cancellation
releases the subscription and closing the runtime stops background tasks.

The observer contract is eventual delivery of the latest committed snapshot,
not delivery of every intermediate UI frame. Never expose rolled-back changes.

## Desktop deliverable

Use Dioxus desktop's native Rust/WebView target on macOS; choose compatible 0.7
dependencies, document the resolved versions and keep them in the lockfile.
Reference the official [desktop guide](https://dioxuslabs.com/learn/0.7/guides/platforms/desktop/)
and [async UI guide](https://dioxuslabs.com/learn/0.7/essentials/basics/async/).
Verify APIs against the selected versions rather than mixing tutorial generations.

Provide a Laptop panel with title input, add, rename, completion and delete.
Creation defaults completion to false. Render keyed by entity identity in stable
order. Feed committed runtime observations into Dioxus; only transient input,
pending activity and error messages are UI-local state. Do not poll SQL on timers.
Database futures/tasks must not block the UI event loop.

Do not author mutations from rendering or automatically restarted read resources.
Once a mutation is admitted, let the runtime finish its commit/rollback even if
the initiating UI task is cancelled; UI cancellation must not leak a transaction.

Provide a main executable with `--data-dir PATH` and the documented default:

```sh
cargo run -p todo-desktop --locked -- --data-dir local/todo-demo
```

Show useful startup/mutation errors. Never erase existing data to fix an error.
Close the runtime cleanly when the app exits. Keep styling simple and local.

## Acceptance checks

- All four actions persist correct canonical and projected state across reopen.
- Counters and identity persist; the next committed operation ID is never reused.
- Inject failure after partial statements but before commit: operation, counters,
  projection, outbox and revision all roll back; no observer success is published.
- Bounded whole-transaction retries do not produce duplicate logical operations.
- Rebuild a fresh projection from retained operations; include tombstones/stamps.
- A mutation between observer registration and initial read is eventually visible.
- A slow observer reaches the latest snapshot with bounded buffering.
- Closing an observer/runtime releases tasks and read snapshots.
- Cancelling a UI waiter leaves admitted mutation work atomic and recoverable.
- Admission rejection and incompatible-file opening preserve existing state.

Use temporary on-disk DBs for persistence tests; in-memory tests cannot establish
restart behavior. Reopen tests establish process/storage behavior, not power-loss
durability. UI verification: create, rename, complete, delete, exit and relaunch
the same directory; verify surviving rows and persistent deletion.

Run targeted runtime/app tests and `./scripts/check`. Document the manual run
result and any platform dependency needed to launch Dioxus.

## Out of scope and handoff

No second replica, network, coordinator, crypto, dynamic SQL write bridge or new
storage backend. Update module/project status and the actual adapter/observer API.
Phase 3 requires reopenable isolated runtime instances, retained original bytes,
durable outgoing work and reusable committed observers.

Next: [two-replica synchronization](03-two-replica-sync.md).
