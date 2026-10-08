# Implemented operation semantics

Phase 1 implements the pure version-1 protocol in `rocky-core` and todo rules in
`todo-app`. Phase 2 adds storage, counters, outgoing work, observation and UI;
see the [local replica guide](../../runtime/local-replica.md). The
[operation contract](../../protocol/operation-contract.md) defines the wire and business
semantics; this page describes the implemented integration surface.

## Fixtures and supported versions

All fixture IDs are 16 raw bytes. Their hexadecimal literals are fixed centrally
in `rocky_core::fixtures` and exercised by the golden frames:

| Identity | Literal |
| --- | --- |
| Mesh | `11111111111111111111111111111111` |
| Todo app | `22222222222222222222222222222222` |
| List/domain | `33333333333333333333333333333333` |
| Owner principal | `44444444444444444444444444444444` |
| Laptop node | `55555555555555555555555555555555` |
| Phone node | `66666666666666666666666666666666` |

`Versions::V1` supports protocol, schema and operation versions independently set
to 1. Any other value returns `UnsupportedVersion` with the field and value.
`todo_app::DEFINITION` declares table ID 1, title field ID 1, completed field ID 2
and the four supported action shapes. These IDs live in the app, not the runtime.

`AuthorSequence` and `LamportTime` support 1 through `i64::MAX`; `LocalRevision`
supports 0 through that maximum. Their checked `new` and `next` methods reject
out-of-range values. `AppliedFrontier` is a sorted per-author contiguous cut,
separate from all three counters. None of these APIs allocate author counters.

## Encoding and validation

`Envelope::encode` and `Envelope::decode` implement the contract field order and
reject noncanonical dependencies, unsupported versions, invalid counters,
truncation and trailing bytes. Dependencies are limited to two IDs. The generic
payload framing ceiling is 65,536 bytes; todo titles have the tighter 1,024-byte
limit. Lengths are checked before allocating variable wire data.

`Action::encode` and `Action::decode` implement the four todo payload tags,
strict booleans and length-prefixed UTF-8. Title bytes, including surrounding
whitespace and Unicode composition, are preserved. Blank titles are rejected.
`TodoApp::validate` also checks the action-specific entity/dependency shape.

Stable envelope hex test vectors are checked in under
[`rocky-core/tests/fixtures`](../../../crates/rocky-core/tests/fixtures):

- `create-v1.hex`: Laptop sequence 1, Lamport 1, title `A`, completed false,
  its own entity ID, no dependencies.
- `title-v1.hex`: Phone sequence 2, Lamport 3, Laptop sequence 1 entity,
  dependencies Laptop sequence 1 and Phone sequence 1, title `B`.

The payload golden bytes are `00010000004100` (create `A`, false),
`010100000042` (title `B`), `0201` (completed true) and `03` (delete).
These fixtures establish wire compatibility, not signatures or authentication.

## Admission, reduction and replay

`StaticPolicy::default` grants the fixture owner whole-row reads and all four
mutations on the fixture scope from Laptop or Phone. Inject a modified policy to
deny an action or title/completed field. `admit_read` checks scope/caller and the
read grant. The node and caller are trusted development inputs.

`TodoApp::apply_ready(state, history, envelope, policy, boundary)` is the pure
combined gateway for local and incoming mutation tests:

1. Validate envelope, payload and dependency shape; check static admission.
2. Classify against `AppliedOperations`: equal original bytes are `Duplicate`,
   different bytes at the same ID are `IdentityConflict`, absent dependencies
   return `Pending` without changing state/history. Known dependency clocks must
   be strictly less than this operation's clock.
3. Reduce ready work and record its canonical bytes as applied. Local writes
   against tombstones return `Tombstoned`; incoming setters are applied no-ops.

`TodoState` stores independent title/completed registers with explicit stamps.
Each greatest `(Lamport, node bytes, author sequence)` wins. A permanent tombstone
replaces the live registers, so hidden values cannot differ with delete/edit
order. Original operations remain in framework history for reconstruction.
`visible_rows` excludes tombstones and sorts by entity ID.

`replay(operations, policy)` builds a fresh state and applied history in canonical
topological order. It rejects conflicting identities, invalid dependencies and
known non-increasing clocks, including clocks of buffered dependencies. If work
cannot become ready, `UnresolvedDependencies` identifies the blocked operation
IDs; no complete projection is returned. Exact duplicates collapse.

## Runtime boundary

`rocky_core::Application` is the installed-app contract: associated state, error
and policy types, plus `validate`, `admit` and `apply_ready`. A generic runtime
can invoke the todo implementation without depending on `todo-app`. Its call
sequence is validation/admission, framework duplicate/readiness classification,
then trait reduction with the local/incoming boundary, and history recording.
The trait reducer assumes admission and readiness were already checked; the
combined todo gateway provides that orchestration for pure phase-1 use.

`AppliedOperations` stores only applied history in memory. The runtime must retain
original validated pending bytes, reject conflicts against pending as well as
applied history, and check clocks against every available dependency. Admission
of a buffered receipt must advance the persisted maximum-known clock; authoring
uses that maximum plus one. Persist author sequence independently.

The runtime must commit history, counters, stamps/tombstones, projection, outbox
and revision together and publish only after commit. On rollback, discard staged
in-memory state/history as well. These pure APIs do not establish DB atomicity.
`StoredApplication` extends the pure interface with compiled projection storage.
See the [local replica guide](../../runtime/local-replica.md) for the implemented adapter and
observer lifecycle.

## Verification

```sh
cargo test -p rocky-core -p todo-app --locked
./scripts/check
```

Tests cover golden frames, malformed inputs, dependency/clock/counter failures,
field winners, static denial and unchanged rejected state. Exhaustive incremental
delivery checks compare full state and applied history over 120 live-edit orders
and 720 delete-race orders, including duplicate deliveries. These checks establish
pure operation behavior; they do not establish persistence or synchronization.

Next: [persistent local replica](../../runtime/local-replica.md).
