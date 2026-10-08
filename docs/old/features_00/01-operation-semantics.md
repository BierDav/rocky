# Phase 1: operation semantics

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented; targeted core/todo tests and shared checks pass. See the
[implemented APIs and handoff](../../applications/todo/operation-semantics.md). Output is a pure operation
model and reducer, not a running UI.

## Goal and required reading

Define a deterministic todo operation family that later storage and transport can
apply without changing business meaning. A replay must reconstruct the same state
from the same accepted operations, even with different ready-delivery orders.

Read [MVP scope and workflow](README.md), [root instructions](../../../AGENTS.md),
[module boundaries](../../modules.md) and the complete
[operation contract](../../protocol/operation-contract.md). That contract fixes action semantics,
dependencies, ordering, encoding and the development admission model.

Architecture context: [application-owned schemas and meaning](../../proposals/architecture-proposal-v3.md#3-applications-own-schemas-and-business-meaning)
and [local mergeable mutations](../../proposals/architecture-proposal-v3.md#local-mergeable-mutations).

## Ownership and prerequisites

- `rocky-core`: typed IDs, envelope metadata, version checks, canonical envelope
  codec, generic dependency/progress types and protocol errors.
- `todo-app`: compiled version-1 app definition, payload codec/validation, todo
  state, reducer, deterministic visible-row view and static permission fixture.
- Keep both independent of Turso, Dioxus and coordinator implementations.
- The baseline declares todo-app -> rocky-core. Preserve that direction. Put the
  smallest shared application interface in core when necessary for phase 2;
  do not make rocky-runtime import todo-app later to avoid defining a boundary.

## Required implementation

1. Define stable fixture identities, version constants and typed error cases.
   Separate author sequence, Lamport clock, local revision and applied frontier.
2. Implement canonical envelope and todo payload encoding/decoding from the
   operation contract. Add checked lengths/counters and golden byte fixtures.
3. Supply a compiled todo app definition with stable table/field IDs, version
   metadata and supported action shapes. It need not parse an arbitrary manifest
   or sign one. The framework must not contain a hardcoded todo field policy.
4. Implement dependency readiness and a pure reducer over ready operations.
   Represent field stamps and tombstones explicitly. Missing creates wait.
5. Provide deterministic visible-row extraction and canonical replay support.
   Replay can topologically process retained operations; report unresolved or
   invalid dependencies rather than silently producing a complete state.
6. Provide static admission hooks and denial fixtures usable at both local and
   incoming boundaries. Caller/node identity remains trusted development input.

Keep public APIs small. Illustrative capabilities are `encode`, `decode`,
`validate`, `apply_ready`, `replay` and `visible_rows`; names are implementation
choices. A reducer must not generate IDs, sample time, perform I/O or notify UI.
Counter allocation and durable dependency buffering belong to the runtime.

## Behavioral examples

Use explicit stamps and fixture IDs so expected winners are independently clear.

| Scenario | Required outcome |
| --- | --- |
| Replay a create twice | One identity, unchanged state |
| Laptop edits title; Phone edits completed | Both field changes survive |
| Both nodes edit title concurrently | Greatest specified stamp wins |
| Equal Lamport values across nodes | Node ID tie-break picks the same winner |
| Setter delivered before its create | Pending until create is applied |
| Delete races with an edit | Todo remains tombstoned |
| Old create/setter replayed after delete | Todo remains invisible |
| Identical ID with a changed payload | Identity conflict, no accepted mutation |

The readiness layer can reject duplicates/identity conflicts before the reducer.
Do not force application state to duplicate the entire framework operation store.

## Acceptance checks

- All supported actions round-trip; golden frames remain byte-for-byte stable.
- Malformed/trailing bytes, unknown versions, oversized or blank titles, invalid
  dependencies and counter overflow produce explicit errors.
- Admissible order permutations and duplicate deliveries produce equal state,
  including merge metadata and tombstones, not just equal visible titles.
- Causal successors outrank their predecessors; concurrent ties are reproducible.
- Separate field updates preserve the untouched field.
- Static policy denies foreign scope/node/caller and disallowed action/field.
- Denied operations leave reducer state unchanged.
- Reducer output and visible-row ordering are stable across fresh instances.

Use small exhaustive permutations or a suitable property-testing library for
convergence. The test oracle should assert the prescribed winners and invariants,
not merely compare two invocations of the same implementation.

Run targeted core/todo tests, then the shared checks:

```sh
cargo test -p rocky-core -p todo-app --locked
./scripts/check
```

## Out of scope

Database storage, observer streams, UI, transport, strong commands, signatures,
encryption, dynamic schema loading, migrations and generalized CRDT selection.
Do not add placeholder storage/network APIs that claim success.

## Handoff to phase 2

Document fixture values, supported versions, encoding test vectors, reducer API
and admission integration in focused docs or concise API documentation. Update
module status to distinguish implemented core/app pieces from empty runtime/UI.

Phase 2 must be able to invoke this app's rules through a shared contract without
depending on todo-specific code inside rocky-runtime. If that interface remains
minimal until integration, describe the intended boundary and existing entry points.

Next: [persistent local replica and UI](02-local-replica-and-ui.md).
