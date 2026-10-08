# Persistent local replica

`rocky-runtime`'s trusted `Replica`: open, write, read, observe, replicate
between named peers and verify a file offline. The
[operation contract](../protocol/operation-contract.md) defines merge and
admission; [write concurrency](write-concurrency.md) defines the engine, the
writer, durability and flushing; [strong publication](strong-publication.md)
adds strong commands. The [app-owned runtime](app-runtime.md) spreads
manifest-bound domains over files; this guide's desktop demo keeps one domain
per file.

## Run the desktop app

```sh
cargo run -p todo-desktop --locked -- --data-dir local/todo-demo
```

The directory (default `local/todo-demo`) holds `laptop.db` and `phone.db` with
fixed fixture identities, their `-wal`/`-shm` sidecars and locks; never remove
sidecars of an open file. Both panels add, rename, complete and delete. Rows
come only from committed observations. **Pause connection** stops delivery while
local edits continue; **Resume connection** reconciles. Each panel reports its
revision and its own operations pending receipt, received by the other peer and
applied there. Closing the window drains both writers. Startup errors go to
stderr; storage and replication errors appear in the panels. Incompatible or
inconsistent data is refused, never deleted or repaired. Only macOS (system
WebView, Dioxus 0.7) is qualified.

## Open and write

`rocky_core::StoredApplication` extends the pure `Application` with a compiled
projection schema, bound projection statements, raw row representation, row
decoding, read admission and the state-window methods `touched` and `window`.
Only the installed application supplies SQL; mutation and UI callers cannot.

`Replica::open(path, app, policy, identity)` opens a local-only domain;
`open_with_config` adds author bindings and named delivery targets. Open
checks the Rocky file tag (`PRAGMA application_id = 0x524b5901`) before
creating a writer or lock, then compares the stored identity record ( mesh, app, domain, node,
caller, descriptor versions, authors, targets, manifest and handler bindings)
before any configuration or schema write and refuses a difference with
`Incompatible`. An existing file must contain all storage tables; missing
metadata is never recreated. Fresh initialization is one transaction, so a
failed one leaves no partial schema.

`Replica::mutate(Mutation { entity, payload })` queues bounded work and returns a
`MutationReceipt`; `entity: None` creates with the allocated operation ID.
Admission, identity and clock allocation, the reducer over the touched rows and
encoding precede one atomic write of operation, projection, outbox, counters and
revision. `receipt.committed()` returns the original envelope after local
commit; it says nothing about peers or flushing. Dropping a receipt does not
cancel admitted work.

`rocky_replica` stores identity, sequence, maximum clock and revision;
`rocky_operations` keeps original canonical bytes and processing metadata;
`rocky_outbox` references local operations until every named target received
them. History behind the horizon is pruned once a verified checkpoint covers it
([checkpoints](checkpoints.md)).

## Queries, observations and shutdown

`Replica::query()` returns `Snapshot { rows, revision }` from one read
transaction bounded to 500 ms. `query()` and `observe()` run on the file's
writer thread between groups, so a slow whole-projection snapshot delays writes;
bound reads use a read-only pool ([paged reads](paged-reads.md)).

`Replica::observe()` registers a bounded watch before its first read.
`Observer::next()` delivers the initial query and requeries after committed
revision changes; frames coalesce to the latest committed state, and cancelling
a pending read cannot lose an invalidation. An observer holds only a weak
runtime reference. `Replica::close()` closes admission, drains accepted
mutations and joins the writer; remaining observers return `Closed`. Dropping
the last handle drains too. A rollback failure stops the writer.

## Replication port

The transport port is `Replica` methods; no transport call runs inside a
transaction.

| Method | Result |
| --- | --- |
| `receive(bytes)` / `receive_page(records)` | Bytes and receipt state committed and flushed; acknowledgment says whether applied ([incoming pages](write-concurrency.md#incoming-pages)) |
| `process_pending()` | Dependency-ready operations applied, at most 32 per transaction |
| `sync_state()` | Held prefix per author, at most 256 retained IDs above it, applied frontier and horizon |
| `missing_operations(known)` | At most 32 records absent from an exact set; cannot signal the horizon |
| `catch_up(peer, state)` / `catch_up_after(peer, state, after)` | At most 32 records the peer lacks with a range cursor, or a `HorizonNotice` |
| `checkpoint_page(cursor)` / `import_checkpoint(page)` | One page of the latest checkpoint; the last imported page switches the projection |
| `acknowledge(peer, ack)` | Received/applied acknowledgment persisted; outbox cleared once all targets received |
| `peer_acknowledgments(peer)`, `peer_progress(peer)` | Retained evidence and the peer's contiguous applied frontier |

`receive` validates frame limits, canonical shape, versions, scope, configured
author and caller and the current write policy. Identical bytes are duplicates;
different bytes for a known ID are conflicts. Unknown operations attributed to
the receiving node are refused: import does not recover a lost author's
storage. Receipt commits bytes, pending status and maximum Lamport time; it
authors nothing and does not advance the revision. Application commits
projection, stamps, tombstones, processing state and revision atomically;
duplicates do not advance the revision, newly applied no-ops do, and observers
wake after commit. A buffered operation later proven invalid becomes rejected:
excluded from receipt sets and frontiers, its bytes kept for conflict detection.

Reconciliation compares per-author held frontiers both ways and pages the gap
([replica roles](replica-roles.md#trusted-path)). It repairs missing
acknowledgments from the receiver's committed state; enqueueing never clears
outgoing work, applied entries are monotonic, and a frontier advances only over
a contiguous prefix. A replica behind the horizon rebootstraps.
`todo_desktop::transport::SimulatedTransport` is the bounded test adapter:
`set_paused`, `reconcile`, `deliver`, `duplicate`, `drop_message` and `clear`
inject delay, reordering, duplicates and loss without sleeps.

## Audit and offline verification

Neither reopen nor audit needs a coordinator. Reopen validates the checkpoint
record, counters, operations after the checkpoint and the strong ledger, then
resumes ready pending work; it does not replay history. `Replica::audit()`
validates every retained identity, author binding, counter and dependency,
recomputes the projection from the checkpoint, compares every stored row with
its digest ([verifying a file](checkpoints.md#verifying-a-file)), and checks
handle bindings and peer acknowledgments. For the strong ledger it checks
request and preparation keys, package and receipt bindings, flags, the
contiguous applied chain from the pruning base, exact replay of cuts the
checkpoint still allows, and outbox consistency. Inconsistency returns
`Corrupt` and is never exposed as confirmed. These checks detect accidental
corruption, not a maliciously rewritten file.

Receipt and package pairs are retained before external verification. An
invalid receipt is moved to the rejection table; the immutable preparation
stays and a later genuine receipt can still verify. Verified evidence is never
overwritten by untrusted input. Coordinator unavailability leaves verification
pending; reconciliation quarantines invalid receipts and continues independent
requests. A replay mismatch in authenticated work is an error.

## Verification

```sh
cargo test -p rocky-runtime -p todo-app -p todo-desktop --locked
cargo run -p todo-desktop --example native_smoke --locked -- local/ui-smoke
cargo run -p todo-desktop --example native_smoke --locked -- local/ui-smoke reopen
```

Use a fresh directory for the first smoke run; it drives the real WebView
through offline edits, competing titles, delete races, close while paused and
relaunch. Runtime tests use temporary files and cover reopen continuity,
rollback without observer wakeup, busy-file retries, observer races and
cancellation, backpressure, shutdown, pending restarts, rejection paths,
reverse, duplicate and dropped delivery and queue loss across restart. They do
not establish networking, authentication, phone background execution or power
loss.

## Design rationale

- **Stored rows are the state.** The runtime holds no complete `State`; each
  operation loads a window of the rows `touched` names, runs the pure reducer on
  it and projects the result. A row-level `load`/`store` interface was rejected:
  it would rewrite every reducer and lose in-memory replay for tests. A
  transition that follows a reference declares both rows or carries the
  reference in its payload. Sealed strong inputs still rebuild the whole cut
  state because the request digest covers it.
- **Reads go through views, not copies.** A bound read's table is replaced by
  the authorized view's `SELECT` as a subquery, compiled under an authorizer
  that permits only the view's source tables. Copying authorized rows into a
  scratch database cost time and memory linear in the domain. A view is
  installed SQL: a wrong filter exposes rows, and the runtime guarantees only
  that a read cannot leave the views.
- **A read is not ordered against queued writes**; it sees what has committed.
