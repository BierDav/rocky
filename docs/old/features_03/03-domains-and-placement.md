# 3. Domains, placement and transaction boundaries

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented on the trusted runtime as a Rust API; see the
[guide](../../runtime/database-bindings.md) and [decision 0017](../../decisions/0017-database-bindings-and-placement-groups.md).
Row-dependent routing is not offered, and cross-file transactions and queries
stay blocked. Owner: `rocky-runtime` catalog/routing/scheduler, `rocky-core`
bindings. Depends on phases [1](01-logical-schema-and-identity.md) and
[2](02-sql-transactions-and-capture.md).

## Distinguish the identities

| Concept | Meaning |
| --- | --- |
| Application definition | Publisher-bound schema, reducer and policy semantics |
| Invariant domain (`Scope`) | Mesh/app/domain boundary for coordination and causal identity |
| Resource scope | Domain plus encryption reader set and manifest table |
| Logical database binding | Tables/resources visible through one authorized SQL session |
| Atomic placement group | Proposed resources whose local SQL commit uses one physical file |
| File slot / physical namespace | Private deployment placement; neither a grant nor business identity |

One database handle may expose several invariant domains only through an explicit
binding. A domain containing several tables is not the same as several domains.
Do not infer domain identity from a filename, SQL primary key or caller-supplied
WHERE clause. Reader scopes and strong domains need not align with file slots.

## Binding and routing

Support a simple generated default: one bound application domain for ordinary
shopping CRUD. Additional domain/table policies belong at database construction.
Bind logical tables to exact resources and reject ambiguous names. Reusing the
same entity schema for another tenant creates another bound handle with the same
DAOs; repositories do not supply a Rocky domain parameter for each method.

Prefer separate bound handles when identical logical table names occur in multiple
domains. A multi-domain combined view needs generated aliases or an explicit
view schema; silently unioning tenant tables breaks keys, counts and authority.
If an application needs row-dependent domain routing in one table, require an
immutable declared routing column/rule and a checked domain set. Domain moves
are explicit migrations, not ordinary updates. Do not require routing callbacks
or repository-side filters.

Persist binding/catalog identity before use. Hot changes invalidate prepared
statements and cursors; no transaction may switch tenant/domain underneath its
existing SQL. Adding or removing an installation must preserve unrelated mappings
and grants and use an explicit generation change.

## Proposed local atomicity model

An atomic placement group contains the projections, protocol rows, transaction
outcome evidence and local-only tables needed by its SQL transaction in **one
file**. All ordinary writable tables in a bound Room database should be placed
in that group at construction. A global installation may still have many groups
and files. Domain independence is retained inside a group.

Implement catalog validation and deterministic lock/lease ordering. Installing a
binding that promises transactions across existing incompatible placements fails
before opening Room. Moving an installed domain is currently unsupported; provide
a durable migration if co-location requires it. Do not silently change the
recorded placement to accommodate a DAO call.

Co-location enables a local multi-domain commit, but does not make peer application
atomic, change independent coordinator heads or grant cross-domain access. Phase 4
must specify per-scope signed bundles and filtered visibility. A same-file group
is insufficient until that protocol passes its own gate.

Independent-file SQL transactions require a separate accepted design. Research
options include changing journal mode with qualified super-journal behavior, or
a durable commit/visibility protocol enforced by every reader and recovery path.
Neither is supplied by current WAL. Two-phase intent records alone cannot stop a
raw reader seeing a partially applied projection. Sequential commits, async
queues, compensations and application sagas are not Room transaction semantics.

Cross-file transactions are a **blocked capability**, not silently supported.
If one bound database must span independently placed files while preserving
arbitrary `@Transaction`, resolve this before the final driver. Revisit the
one-file proposal if it does not fit the required application; do not turn the
restriction into repository boilerplate.

## Queries and partial replicas

Same-file multi-domain queries execute over the session's authorized resources.
Different-file queries need bounded federation with an explicit snapshot contract
or co-location; opening every file with `ATTACH` cannot cover the 256-slot layout.
Phase 5 owns query planning and consistency. Arbitrary cross-file joins and
cross-file relation-loading transactions remain blocked until that contract works.

Placement selects held trusted domains; secure subscriptions select held resource
scopes. Missing installations and evicted/unsubscribed scopes are unavailable.
The binding cannot convert them to empty SQL tables. Remote reads remain an
explicit source with provenance; normal DAO execution performs no hidden network
calls while holding a transaction.

## Completion gate

- Two handles with identical table names/keys in different tenants never leak or
  overwrite each other, including joins, aggregates, updates and prepared caches.
- One-file multi-domain transactions atomically commit protocol and projections;
  mixed denied domains roll back together.
- Catalog interruptions, selected-file opens, concurrent disjoint groups and
  placement migration preserve mappings and prevent two writers owning one file.
- Exercise the existing 4,096-domain/256-slot supported limits without attaching
  every file or materializing every domain's data in memory.
- State the proven cross-file read/write capabilities and keep unsupported ones
  blocked at binding/planning. Same-file tests cannot qualify them.

Use [wide schemas](../../runtime/wide-schemas.md), [decision 0013](../../decisions/0013-wide-schema-placement.md)
and [replica roles](../../runtime/replica-roles.md) as existing contracts. Run targeted
placement/crash tests and `./scripts/check`; new file semantics require an ADR.
