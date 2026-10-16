# Supported runtime API

The trusted `rocky_runtime::Replica` facade supports one compiled domain per
file. The [app-owned runtime](app-runtime.md) adds manifest-bound shared files,
general strong commands, authorized bound/typed reads and payload-bound strong
reads. See [local replicas](local-replica.md) for desktop setup and
[audit recovery](local-replica.md#audit-and-offline-verification) for ledger validation.

## Install an application

Implement `Application` for deterministic validation/reduction and
`StoredApplication` for compiled schema/projection/row decoding. Supply an
`ApplicationDescriptor`: stable definition identity, operation versions and
projection generation. Change the definition identity when reducer or codec
semantics change. Schema SQL, projection query and registered handler generations
are also fingerprinted. SQL comes only from the installed app.

`project(state, operation)` receives the state immediately after that operation.
Incoming batches retain these statements in transition order and execute them
in the same transaction as history and revision changes. Projection statements
must reconstruct the same final stored representation as `projection_rows`.
Reducers must converge over allowed dependency-ready delivery orders. A projection
that records intermediate values must make those values deterministic too.

`admit_candidate(before, after, operation, context, policy)` can reject the entire
candidate transition at local and incoming apply boundaries. Context carries
scope, caller, policy version and boundary. Applications check paired row/field
grants against both states; permitting one row and a field on another does not
authorize a mixed patch. Todo supplies optional `RowGrant` selections; the ledger
example rejects ownership changes and negative balances. Receive checks static
admission, and pending application uses the currently installed policy. Reopen
replays already-applied operations without making a new authorization decision.
`select_rows` selects complete visible rows under current policy.

Commands are optional. `commands()` registers method/version/schema/handler
identities, one entry per version of a method the application still accepts;
a request of another version is refused, never translated. `command(method)`
supplies a `CommandHandler`. Its pure `validate`,
`seal` and `execute` functions define deterministic exact-cut results; `admit`
authorizes new invocation/execution. A merge-only app implements neither method.
`select_result` selects complete immutable publications under current read policy.
The result is immutable application-owned bytes. `execute_after` and
`project_result` support [predecessor-based transitions and atomic job intents](app-runtime.md#invoke-and-apply-general-commands). Todo composition selects
`todo_app::PUBLISH_LIST_SNAPSHOT` through `request_command`.

A compiling second app has two projection tables and state-aware policy:

```sh
cargo run -p rocky-runtime --example ledger --locked
```

Its implementation is [ledger_app.rs](../../crates/rocky-runtime/examples/support/ledger_app.rs).

## Configure trusted development identities

`Replica::open` admits only its supplied local node/principal and has no outgoing
delivery obligations. For replication use `open_with_config` and an explicit
`RuntimeConfig` containing author `(NodeId, PrincipalId)` bindings and named
`delivery_targets`. The same bindings are checked at receive and reopen. Changing
the configuration requires a new file in this version. These are trusted inputs,
not authenticated transport, signatures or delegation.

Each outgoing operation stays obligated until every configured target has durable
receipt. Results stay obligated until every target reports application. Acks are
monotonic and independently identified by peer. Historical records remain after
outbox delivery; one peer cannot clear another's obligation.

## Submit, cancel and recover

`mutate` queues a precise action and returns a `MutationReceipt` with a stable
`MutationHandle`. `committed()` waits for the local commit, which is buffered by
default and can disappear after power loss until a flush covers it.
`receipt()` returns a `CommittedMutation` with its `CommitClass` (`Buffered` or
`Flushed`); `flushed()` waits for a flush and forces one if needed. Save the
handle before dropping/cancelling the waiter; `lookup_mutation(handle)` returns
the original envelope after commit, including after reopen, and `lookup_commit`
adds its class. `None` means no commit evidence: the operation may still be
queued, may have been rejected, or was buffered and lost to power loss. Retrying
the handle is safe in every case. Enqueue success alone is not acceptance.
Dropping a waiter does not cancel runtime-owned work. `mutate` returns
`Backpressure` at once when the domain's queue is full; `submit` and
`submit_with_handle` wait for room instead, and `queue_depth` reports the
commands and bytes waiting ([write concurrency](write-concurrency.md)).

Nothing reaches a peer or the coordinator through Rocky's APIs before a flush
covers it: replication pages, acknowledgments and published commands wait for
one. Do not send the envelope from `committed()` to a peer yourself; use the
replication APIs or await `flushed()` first. `Replica::flush()` waits until a
flush covers every commit of the domain. `RuntimeOptions::durability` set to
`Durability::Full` flushes every commit instead
([design rationale](write-concurrency.md#durability-and-exposure)).

`mutate_with_handle` supports explicit idempotent resubmission. Reusing a handle
with different entity/payload returns `Conflict`. `mutate_with_precondition`
optionally requires a matching local revision before a new mutation. Retrying an
already committed handle returns its original result despite a newer revision.

`submit_command(CommandHandle, method)` returns a `CommandReceipt` immediately;
`committed()` waits for the durable sealed `StrongRequest`, not coordinator
confirmation. `lookup_command` and resubmitting the same handle recover that
original request. Different methods conflict. `request_command` is the convenience
form that allocates a handle and waits. Tracking the returned request identity
through `strong_status` distinguishes queued/unknown/local applied and
peer-applied evidence. Verification states and named peer acknowledgments are
reported separately. A timeout after submission means unknown confirmation;
lookup the original request before resubmitting to the coordinator.

`Error::reason()` provides stable categories: denial, unsupported generation,
missing dependencies/head, quota, corruption, conflict, closed, unknown confirmation
and storage failure. `PendingReason` refines pending recovery actions. Error sources
are retained through `std::error::Error::source`; SDK consumers need not parse
engine strings. `is_retryable()` distinguishes transient backpressure, busy retries,
coordination outages/fencing and pending dependencies from terminal failures.
Quota exhaustion of retained history requires another domain file, not blind
retry or deleting unresolved data.

## Read and observe

`query()` returns the installed compiled row query: `QueryIdentity` (scope and
installed definition), rows, applied frontier, local revision and `LocalStrongHead`.
It copies no publication packages. These are local snapshots and can be stale;
neither a frontier nor a local strong head proves network completeness or a
coordinator-bound strong read. Queries select whole rows under current policy.

`query()` and `observe()` return the whole projection and still run on the
writer thread; for large domains use the paged, indexed
[bound reads](app-runtime.md#read-authorized-local-data).

`observe()` is fallible and registers before its initial query. `next()` coalesces
committed revisions and returns the same scoped snapshot. Cancellation retains the
last delivered revision so a later `next()` can obtain current state. Drop the
observer to release its budget. There are no per-observer background tasks.

Use `confirmed_publications` for explicit locally applied history and
`strong_results` for retained receipt/package transfer, including pending verification.
`operation_page`, `request_page` and `result_page` provide ordered continuation
pages. Consume `items`, then pass `next` until it is `None`. Each page caps records
and bytes. Restart reconciliation to discover new keys inserted before a cursor.
Records behind the [pruned prefix](checkpoints.md) are no longer returned; use
`catch_up` to learn that a peer needs a checkpoint. The convenience exact receipt/status/
history APIs are bounded by the hard retention ceilings below.

## Budgets and scheduling

| Resource | Limit |
| --- | --- |
| Queued commands | 32 |
| Queue accounted payload/metadata bytes | 8 MiB |
| Command/frame including queue metadata | 1 MiB |
| Retained operation bytes (`RetentionPolicy::retained_bytes`) | 64 MiB by default; a checkpoint starts at half, `Error::Retention` at the bound |
| Retained strong bytes | The same bound, counted separately |
| Operations kept behind a checkpoint (`window`) | 1,024 per author by default |
| Handles kept for retries (`handles`) | 65,536 by default |
| Page/pass records | 32 |
| Page bytes | 1 MiB |
| Bound read page | 1,024 rows and 1 MiB, continued by a [keyset or offset cursor](paged-reads.md) |
| Observers | 128 |
| Configured author bindings / targets | 64 each |
| Writing steps per group commit (`group_commit`) | 128 by default, 1 to 256 |
| WAL frames before a checkpoint (`checkpoint_frames`) | 16,384 (64 MiB) by default, 256 to 1,048,576 |
| Queued bytes per group commit | 4 MiB |
| Files per `Runtime` (`files`) | 1 by default, 1 to 16 |
| Read transaction | 500 ms, then a retryable error |

Limits are checked before retaining new work, including pending/rejected recovery
evidence. History is released only by [pruning](checkpoints.md), never to make
room for an admission. Counters, projection,
outbox and handles remain unchanged when admission fails. `process_pending` yields
between worker passes so queued queries/close can run. Close has no byte-budget
charge and drains queued accepted commands before releasing the file. Dependency
pending operations remain durable for a future processing pass.

Private file ownership holds the database and exclusive lock; separate scoped
worker state holds author/revision/history and serial execution. `Identity::author_key`
and `Snapshot::revision_key` expose scope-bound keys; revision numbers from different
keys are not comparable. The public domain
API has no shared mutable cursor. `Runtime` adds declared domains with separate
table namespaces, spread over up to 16 files with one writer each
([app runtime](app-runtime.md#install-a-definition-and-domains)); `Replica::open`
is the single-domain, single-file form. See
[qualification](app-runtime.md#examples-and-qualification).
