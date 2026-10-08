# App-owned runtime

The trusted `rocky-runtime` boundary for compiled applications: manifest-bound
domains in shared files, general strong commands and authorized reads. It
extends the single-domain [supported API](supported-api.md), which lists the
budgets. The encrypted profile is separate ([SDK composition](sdk-composition.md)).

## Install a definition and domains

`StoredApplication::manifest()` supplies an unsigned `ApplicationManifest` body.
Publisher bytes and app name derive the stable app ID; kind/name/app/publisher
bind entity, field, method and operation IDs. Shapes and domain rules are canonical
UTF-8 descriptions of the installed semantics. Schema, projection and handler
versions are independent. The manifest declares domain-scoped author logs.

`canonical_body`, `from_body`, `validate` and `hash` define bounded,
language-independent definition data. Counts/lengths/versions are little-endian
u32; IDs are fixed bytes; groups are sorted by ID. Decode requires canonical
order and rejects duplicates, unsupported log scopes, truncation and trailing data.
The body carries no executable code and does not load or register handlers.
Publisher identity is a namespace binding here; signing and authentication
belong to the encrypted profile.

`Runtime::open(path, Vec<DomainInstallation<A>>)` installs one compiled app type
in 1–4,096 declared domains. Each installation supplies its app, current policy,
identity and trusted node/principal/delivery configuration. Runtime checks manifest
hashes, domain declarations and installed handler/schema/projection generations.
The immutable file binding also fingerprints compiled schema and method registration.
The automatic layout binds the complete installation set. [Explicit placement
and additive indexes](wide-schemas.md) permit adding assigned domains and non-unique
indexes while keeping existing domain identities.

The runtime spreads its domains over `RuntimeOptions::files` SQLite files in WAL
mode, 1 by default and at most 16, using no more files than domains
([design rationale](write-concurrency.md#engine) and
[0008](write-concurrency.md#durability-and-exposure)). File 0 is the
given path and file `k` adds `-k` to its stem (`app.db`, `app-1.db`); domain `i`
in sorted scope order lives in file `i % files`. Each file has its own lock.
Each domain owns a bounded queue, reducer, history, counters, outboxes and
revision row in a physical table namespace. Each file records the binding and
the layout and commits its initial schemas in one transaction; file 0 is
installed last, so an interrupted install resumes. Reopen checks bindings and
layout before changing configuration or schema: another file count, or a
missing, extra or foreign file, is refused with `Incompatible`. The installation
rows are immutable; ordinary work never updates a global revision row.

`runtime.domain(scope)` returns a cloneable domain handle. Domain work serializes
conflicting author and command transitions. Each file has one writer thread that
owns its only write connection and commits the writes waiting across its domains
in one transaction, one savepoint per step
([local replica](local-replica.md#open-and-write)). Reads use a
read-only connection on the same thread. Workers are cooperative tasks on that
thread, so a file's reducers and replay do not run in parallel; different files
run in parallel. No transaction spans two files.
`runtime.close()` drains domains and joins every file writer; closing one domain
alone does not release a file still owned by another domain.

Commits are buffered by default and flushed before anything leaves the device;
`Durability::Full` flushes every group. Full durability is fastest in one file
(`files: 1`), buffered writes in several
([scaling limits](scaling-limits.md#measured-limits)).

`Envelope::scoped_id()` and `scoped_dependencies()` expose complete operation
identities. Wire envelopes bind the scope once; compact IDs and causal references
are relative to that declared domain log. Sequence 1 can exist in two domains on
one node. Use `mutate_scoped` in app SDKs to reject foreign entity references.
`LocalRevision` is comparable only within its `RevisionKey`.

## Invoke and apply general commands

`invoke_command(handle, CommandInvocation)` binds caller-supplied handle,
method/version, arguments, policy reference and exact sealed input frontier/digest.
With `inputs: None`, durable acceptance seals the current applied mergeable cut;
explicit `SealedInputs` can name inputs not yet received. Request bytes retain the
caller-supplied handle, and coordinator commitments cover it. Handle recovery is
scoped to the originating node/domain; sharing the same handle across independent
origin nodes is not a global idempotency service.

A duplicate handle returns its original durable request. Changed method/version,
arguments, policy or explicitly supplied input cut returns `Conflict`. Dropping
the submission waiter preserves runtime-owned queued work. `lookup_command`
recovers durable evidence; enqueue success alone does not establish durability.

Installed `CommandHandler::execute_after` receives a recovered confirmed
predecessor result and the state reconstructed from the sealed mergeable cut.
Predecessor state is separate from the current draft projection. Handlers encode
business rejection as deterministic result bytes; authorization, framing or
unsupported generations remain errors. Every decision advances the confirmed
chain, including business rejection.

Preparation stores immutable output without changing projections. External
confirmation and receipt verification stay outside database transactions.
Application replays the input/predecessor, checks the result, then commits
`project_result` statements, job intents, applied flags, strong head, outbox and
scope revision together. Mutation, every strong step and result application retry
whole transactions up to ten times with stable identities
([helper](local-replica.md#open-and-write)). Notifications follow successful commit.
`result_projection_query/rows` let reopen verify persisted strong state/jobs against
confirmed replay. Job intent storage does not execute jobs; see [effects](recovery-and-effects.md).

## Read authorized local data

App authors implement `read_views(context, policy)`. A `ReadView` is a name, a
`SELECT` over the application's stored tables and its bound values; the policy
filter is part of that `SELECT`. `read(BoundQuery, ReadOptions)` validates the
bound SQL, replaces its single table reference by the view's `SELECT` and runs
it against stored rows in one read transaction on a read-only connection, off
the writer thread. Nothing is copied. The caller's text is never mapped to a
physical table, and the statement is compiled under an authorizer that permits
only the view's source tables, so denied rows, other namespaces and protocol
tables stay unreachable. A wrong filter in a view exposes rows: views are
installed, trusted SQL ([design rationale](local-replica.md#design-rationale)).

Reads return scope, the committed revision of that transaction, local
dependency-backlog completeness and strong head. A read sees what is committed
when it runs; it does not wait for queued writes of its domain.

The supported dynamic language is a single SELECT over one view, with bound
parameters, filtering, ordering, grouping and integer aggregates. Subqueries,
joins, CTEs, unions, comments, multiple statements, dotted identifiers,
protocol/schema tables, extension/file functions, writes, DDL and ATTACH are
rejected. Only COUNT/SUM/MIN/MAX/LENGTH/COALESCE functions are accepted. SQL is
limited to 8 KiB. REAL results are returned as canonical doubles; an
expression that yields NaN is rejected.

A read returns one page: at most `max_rows` (1 to 1,024) rows and `max_bytes`
(at most 1 MiB). `ReadSnapshot::next` is set when more remain; pass it with
`ReadOptions::after`. A cursor is bound to its scope, SQL and values and is
refused for another query. Pages of one pass may come from different revisions;
each reports its own.

What a deep page costs depends on the cursor's kind. A view that declares
`ReadView::key` gives ordered plain queries a keyset cursor, which seeks and
costs the same at any depth. Every other read gets an offset cursor, which
skips the rows already read. Declare a key for every view that can hold many
rows and read [paged reads](paged-reads.md) for the rules and the trade-off.

Applications declare indexes in `schema()`; the installed fingerprint covers
them. The view is flattened into the query, so those indexes serve point and
range reads. `Replica::explain_read` returns the plan for tests.

`TypedQuery` and `read_typed` provide app-owned decoding and a `TypedSnapshot<R>`
without exposing engine values to UI code. `observe_read` registers before its
first read. The worker records which stored tables each commit wrote; an
observer wakes only when a source table of its view changed, or when
completeness or the strong head may have changed. Observers of the same query
share one execution per change, and slow observers coalesce. Other domains do
not wake it. Cancellation does not consume the last delivered generation.
Observers use the existing 128-per-domain budget and create no background tasks
or remote subscriptions. Measurements are in
[scaling limits](scaling-limits.md#measured-limits).

## Discover, fetch and track

`read_strong` discovers a coordinator head, reconciles retained payloads and
requires the exact applied configuration/sequence/head. `Ready` identifies that
discovered head; `Pending` distinguishes missing packages and missing inputs;
`Unavailable` reports coordination outage. This is a head discovered for this
read, not a guarantee that no later remote commit exists.

`read_strong_from` also fetches the matching confirmed chain and required operations
through `StrongPayloadSource`, verifies receipts, applies inputs/results and
rechecks discovery. `Replica<A>` implements this real transport replacement
boundary for a connected retained-history source. Network adapters can return
missing payloads. Fetch is bounded to 256 results per pass and creates no ongoing
subscription. Transport and coordinator calls never hold database transactions.

`track_mutation` and `track_command` report evidence for separate accepted handles
and explicit peers. Command tracking includes its recorded authorized outcome;
peer receipt/application evidence remains independent. `observe_progress`
coalesces durable progress without rendering a row query. Missing durable evidence
means queued, rejected or unknown rather than invented success. Errors expose
stable `FailureReason`, `PendingReason`, retryability and opaque storage sources.

## Examples and qualification

```sh
cargo run -p rocky-runtime --example inventory --locked
cargo run -p rocky-runtime --example ledger --locked
```

[InventoryApp](../../examples/inventory-app/src/lib.rs) demonstrates app-author
reducers, manifest, strong transition and job projection. The
[typed inventory client](../../crates/rocky-runtime/examples/support/inventory_client.rs)
and [composition example](../../crates/rocky-runtime/examples/inventory.rs) show UI
consumers supplying note/reservation actions without SQL writes or strong packages.

`Replica::open` serves one domain per file with optional manifests; shared
`Runtime::open` requires manifests. A file of any other format or layout is
refused with `Incompatible` and left unchanged. The todo desktop uses one
file per replica.

`cargo test -p rocky-runtime` covers this boundary (`app_runtime_tests.rs`,
`read_tests.rs`, `strong_tests.rs`): two domains in one file with independent
sequences across reopen; whole-transaction retry and rollback of projection,
history, outbox, counters and handles; duplicate-handle recovery and conflicts;
reordered confirmed results waiting for predecessors; rejected dynamic SQL;
coalesced observation. `rocky-core/tests/manifest.rs` covers canonical manifest
bodies, truncation, trailing data and duplicate IDs.

## Design rationale

- Each domain has a physical table namespace, so compiled application SQL and
  one atomic projection/history/outbox transaction per domain survive sharing
  a file. Operation identities include the domain; wire references stay
  relative to the enclosing envelope scope.
- Reads run as installed, trusted views flattened into the caller's query on
  stored rows. Copying authorized rows into isolated in-memory databases was
  replaced because it cost a copy per read.
- Handlers are pure functions of the predecessor result and the sealed
  mergeable cut; projections and job intents apply atomically only after
  confirmation, so external calls never hold a database transaction.
