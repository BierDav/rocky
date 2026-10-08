# MVP operation contract

Status: pure protocol, codec, admission and reducer rules are implemented in
phase 1. Local transaction/storage/observation rules are implemented in phase 2;
remote processing and transport are implemented in phase 3.
Also used by phase 4 input sealing. See [implemented APIs](../applications/todo/operation-semantics.md) for the trusted-development assumptions.

## Scope, identities and versions

Use typed mesh, app, domain, node and principal IDs with 16-byte values. Define
stable fixtures centrally: distinct IDs for the mesh, todo app, list/domain,
owner principal, Laptop node and Phone node. Document their literal values in
the implementation tests; reopening a DB must not generate a new node identity.

An operation ID is `(author_node_id, author_sequence)`, scoped by mesh/app/domain.
Sequences start at 1 and form a contiguous committed author log. A todo/entity
ID equals its CreateTodo operation ID. Never reuse a committed creation identity.
Node sequence and Lamport time are separate counters; both persist in phase 2.

Protocol, logical schema and operation versions start at 1 and are separate
fields. Unknown versions fail with an explicit unsupported-version error.
Counters use unsigned 64-bit wire values; the MVP supports at most `i64::MAX`
for compatibility with SQL integer storage. Overflow fails instead of wrapping.

## Envelope and actions

An envelope carries mesh/app/domain, caller principal, protocol/schema/operation
versions, operation ID, Lamport time, entity ID, dependencies and payload bytes.
The runtime validates this envelope; todo payload validation belongs to todo-app.

| Action | Payload | Meaning |
| --- | --- | --- |
| CreateTodo | Title and initial completed flag | Create a new identity and initialize both fields |
| SetTitle | Title | Replace only the title register |
| SetCompleted | Boolean | Replace only the completion register |
| DeleteTodo | Empty | Permanently tombstone this identity |

Titles must contain a non-whitespace character and be at most 1,024 UTF-8 bytes.
Accepted bytes are preserved exactly; UI trimming happens before authoring.
Do not normalize Unicode during remote replay. UI creation defaults completed
to false, but that value is explicitly recorded in the operation.

Do not replicate toggle, full-row replacement, SQL or UI component state.

## Dependency and clock rules

- Every operation after author sequence 1 depends on that author's predecessor.
- Every non-create operation also depends on the entity's CreateTodo operation.
  Store the sorted unique union of these dependencies, at most two IDs.
- CreateTodo must use its own operation ID as its entity ID. Validate dependency
  shape, reject self-dependency and impossible own-author forward references.
- An operation is ready only when all dependencies are applied. Missing
  dependencies are pending, not an implicit row creation or deletion.
- A setter targeting a known tombstone is an applied no-op; preserve its history.
  Ordinary local UI mutations against a tombstone should return an error.
- Persist the maximum Lamport time of admitted received operations, including
  buffered ones. Author a local operation at maximum-known time plus one.
- Validate increasing Lamport order against declared dependencies when available.
  Invalid operations are rejected, not acknowledged as applied.

Because logs have contiguous predecessor dependencies, an applied frontier is
a sorted vector of `(author_node_id, highest_contiguous_applied_sequence)`.
It describes this replica's applied cut, not unknown operations on offline nodes.
Never substitute the largest globally observed operation number for this vector.

## Deterministic merge

The register stamp is `(lamport_time, author_node_id, author_sequence)`, compared
lexicographically using numeric counter order and bytewise node ID order.
The greatest stamp wins separately for title and completed. Creation supplies
the initial register values and stamps. Delete wins permanently for its identity,
regardless of competing setter stamps; retain a tombstone through rebuild/replay.

Ready operations can arrive in different orders. Duplicate application changes
nothing. Two concurrent edits of one field pick a deterministic winner; edits
of different fields survive independently. Text editing is not a text CRDT.

Visible rows exclude tombstones and sort by entity ID. Preserve register stamps
and tombstones internally. Query and snapshot ordering must not depend on map
iteration or SQL's unspecified row order.

## Stable encoding

Use a small versioned deterministic binary codec, not default struct/debug output.
Encode IDs as raw fixed bytes, counters as little-endian u64, versions and lengths
as little-endian u32, booleans as exactly 0 or 1, and text as length-prefixed UTF-8.
Encode an operation ID as node bytes followed by its sequence.

Envelope field order: mesh, app, domain, caller, protocol version, schema version,
operation version, operation ID, Lamport time, entity ID, dependency count,
sorted dependency IDs, payload byte length, payload bytes.

Todo payload starts with a one-byte tag: 0=create, 1=title, 2=completed, 3=delete.
Create encodes title then initial boolean; title encodes its string; completed
encodes its boolean; delete has no further bytes. Reject trailing bytes, invalid
booleans, invalid UTF-8, invalid counts and lengths beyond allowed bounds before
allocating. Envelope dependencies are canonical sorted unique values.

Retain the original validated canonical bytes. The same operation ID with the
same bytes is a duplicate; the same ID with different bytes is an identity conflict
and must be rejected. Golden byte fixtures establish version-1 compatibility.

## Static admission

The fixture owner has whole-row read plus explicit create, title/completion update
and delete permissions for this list. Only the two configured node IDs may author
operations. Check mesh/app/domain, caller, action, fields and versions at local
mutation and incoming processing; neither path may bypass admission.

Provide an injectable static policy fixture for denied-operation tests. Rejection
must be whole-operation rejection with no projection or outgoing changes. These
checks establish boundary behavior, not signed delegation or authenticated authors.

## Transaction and progress rules

Local commit atomically persists operation bytes, author counters, merge metadata,
projection, outbox and per-domain observer revision. Received processing persists
bytes, pending/applied status, clock and any applied changes in the corresponding
local transaction. A buffered receipt must survive restart.

An observer revision is local and per-domain; it is not an operation ID or remote
frontier. Advance it once for an apply transaction containing newly applied work,
including winning-value no-ops; exact revision counts are not cross-node contracts.
Duplicates do not advance it. Publish wakeups only after commit.

Report local durable, peer durably received and peer applied separately. Transport
delivery alone proves neither durable receipt nor application.

## References

- [Application-owned meaning](../proposals/architecture-proposal-v3.md#3-applications-own-schemas-and-business-meaning)
- [Mutation paths](../proposals/architecture-proposal-v3.md#5-two-mutation-paths)
- [Runtime transaction ownership](../proposals/architecture-proposal-v3.md#11-runtime-ownership-and-transaction-boundaries)
- [Observation and progress](../proposals/architecture-proposal-v3.md#13-sdks-sql-reads-and-observation)
