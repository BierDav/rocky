# 1. Logical schema and entity identity

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented on the trusted runtime as a Rust API, with auto-generated
keys left as an explicit blocker; see the [guide](../../runtime/logical-schema.md)
and [decision 0015](../../decisions/0015-logical-schema-and-row-identity.md).
Owner: `rocky-core` schema/identity types, `rocky-runtime`
installation/projection, `rocky-sdk` generation. Depends on existing manifests.

## Problem

Room entities have typed columns and application-owned primary keys. The current
SDK exposes collection JSON and creation-operation identities. Its Kotlin schema
duplicates Room's entity definitions. The static `StoredApplication` contract is
a compiled-app boundary, not an owned dynamic SQL-schema descriptor.

## Implement

Generate one canonical schema descriptor from Room's exported schema or a build
processor. Include table/column identities, SQL names, affinity, nullability,
default expressions, primary-key order, indexes, foreign keys, views and derived
columns. Room's schema identity and Rocky's signed replicated definition remain
separate fingerprints with a verified correspondence. Generation is SDK-owned;
applications provide exceptional policies once, at setup, not row mappings.

Introduce an owned installed-schema contract for generated SQL without leaking
strings to satisfy static lifetimes. Preserve compiled adapters through a real
replacement boundary. Install real typed projections with hidden protocol state
in separate private tables. Public schema introspection exposes exactly the
logical descriptor, validated against the installed physical projection.

Distinguish logical table/field IDs, SQL spelling, domain identity, business
primary key, row incarnation and operation identity. Physical name prefixes
and file positions must not enter replicated row identity. Persist the logical
mapping and reject a mismatched binding on reopen.

Canonical key encoding must include types, component lengths, table/domain
binding and comparison rules. Compound keys, BLOBs, Unicode, signed 64-bit values
and SQLite collation/affinity behavior need an explicit contract. Byte equality
alone cannot reproduce every SQLite key comparison. Key mutation and table/field
renames require versioned identity-preserving or identity-changing rules.

Specify concurrent insertion at the same business key, delete/reinsert,
`INSERT OR REPLACE` and `@Upsert`. Existing permanent tombstones apply to an
incarnation, not silently to every future business-key reuse. Choose deterministic
incarnation selection and retain causal evidence so an old update cannot resurrect
a deleted incarnation or attach to its replacement. This is a protocol change.

Provide offline-safe Room integer auto-generation, or identify it as a blocking
compatibility gap. Candidate designs include enrolled node/range allocations;
prove collision avoidance, exhaustion and rollback/recovery behavior before
selection. Do not expose operation-ID strings, remap returned keys later, or
silently change existing DAO return types. Existing arbitrary keys also require
deterministic handling of concurrent collisions.

Complete the public scalar codec: NULL, INTEGER, REAL, TEXT and BLOB with precise
transport encoding and SQLite getter conversions. The engine already supports
REAL; shared `SqlValue`/SDK codecs do not. Bind float canonicalization, signed
zero, non-finite input behavior and integer precision in the definition. Room
type converters remain ordinary application code before binding/after reading.

## Schema policy

Classify tables/columns as mergeable, local-only, derived, confirmed read-only or
command-controlled using generated setup metadata. Defaults must not accidentally
replicate Room bookkeeping, authentication secrets or derived indexes. All synced
base data has one canonical replicated representation; a separately rebuilt
projection is acceptable only with atomic runtime maintenance, not a Room cache
and refresh queue.

Constraints divide into local SQL checks and globally required invariants.
Secondary uniqueness, CHECK predicates over concurrently merged fields and
cross-row foreign keys need a deterministic conflict/materialization policy or
strong commands. An incoming operation cannot remain permanently unapplicable
merely because two independently valid offline writes collide. Phase 7 handles
strong state; this phase must identify incompatible mergeable schemas at install.

For declared local-only/derived fields such as shopping's `searchTitle`, specify
how they survive canonical updates, deletion, rebuild and migration. A derived
search value must be recomputed by one installed rule at runtime, not maintained
through a repository-side projection callback. Register semantics do not infer
counter, set or business-transition intent from SQL expressions.

## Completion gate

- Generate the shopping schema without manual `Table`/`Column` duplication.
- Preserve application keys and typed DAO shapes through restart, delivery
  reordering, concurrent key insertion, deletion and replacement.
- Demonstrate compound/BLOB keys, collation edge cases, integer limits, REAL and
  converter-backed columns against the pinned SQLite behavior.
- Auto-generated key behavior is implemented and tested, or remains an explicit
  blocker for applications using it; no blanket compatibility claim is allowed.
- Verify metadata against actual tables/indexes and refuse corrupt/incompatible
  descriptors, publisher bindings and key/field codecs.

Record schema/identity/format decisions before implementation. Run targeted
`rocky-core`, `rocky-sdk` and `rocky-runtime` checks and `./scripts/check`.
See [transactions](02-sql-transactions-and-capture.md) and
[migrations](06-migrations-and-recovery.md) for consumers of this contract.
