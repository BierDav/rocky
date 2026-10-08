# 12. Hidden rows, unique indexes and foreign keys

Status: complete; qualified on 2026-10-08. Owner: `rocky-secure` merge and typed-table projection,
`rocky-core` descriptor, the Kotlin builder. Depends on
[phase 2](02-schema-builder-and-room-import.md) and
[phase 7](07-row-identity.md). Wave 2. Cross-scope references, the existence
log and puppets are [phase 14](14-existence-log-and-puppets.md).
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 5](agreed-scope.md#5-constraints-in-mergeable-tables) of the
scope up to and including [foreign keys](agreed-scope.md#foreign-keys),
[secure SQL](../runtime/secure-sql.md) and the primary-key clash handling
that exists today. Unique indexes and foreign keys are refused in mergeable
tables; the installation diagnostics show where.

Rocky keeps every merged row in its private state and writes only the
resolved result into the typed table, so the real index and foreign key
always hold. A row kept out is hidden, a pure function of the merged rows,
independent of arrival order. These rules give convergence, not invariants.

## Ordered slices

1. Decision record: constraint policies and delete actions in the schema
   descriptor, the hidden state, versioning and refusal.
2. Hidden rows: resolution as a pure function, re-evaluated when any input
   row changes; a hidden row returns when its cause goes away; no conflict
   table; invisible to the application.
3. Unique indexes: must include the partition columns; later stamp wins;
   `SetNull` (indexed non-partition columns read NULL, must be nullable) and
   `LatestWins` (whole row hidden). Partial index on `row_kind = 0`.
4. Foreign keys within one scope: reference the parent's identity index;
   foreign-key columns merge as one unit; `CASCADE` hides the child and
   `SET NULL` reads NULL (nullable, outside the primary key); `RESTRICT` and
   natural-key references are refused at installation. A child arriving before
   its parent is hidden until it arrives; bundles apply with deferred checks.
5. Authoring device: the constraint is a real SQLite constraint, so a local
   duplicate or dangling reference fails at once.
6. Builder: `unique(…, onConflict)` and `reference(…, onDelete)` in the
   Kotlin builder and from Room's export.

## Completion gate

- Two offline inserts with one unique value converge to the same visible row
  on every replica regardless of arrival order; the loser reappears when the
  winner changes its value.
- A parent delete concurrent with a child insert leaves the child hidden on
  every replica; repointing the child to a live parent brings it back.
- A local duplicate fails at the statement; a remote one never raises.
- A re-created parent has a new `id` and the old children stay orphans.
- [Secure SQL](../runtime/secure-sql.md) documents policies and delete actions.

## Implementation and qualification

All six slices are implemented. [Decision 0035](../decisions/0035-hidden-rows-unique-indexes-and-foreign-keys.md)
binds policies and delete actions into definition/descriptor data while preserving
unconstrained format-2 bytes. `rocky-secure` keeps private candidate tables and
resolves visibility from merged state after authoring, remote apply, reopen,
checkpoint bootstrap and eviction. Real typed-table constraints remain intact.
The trusted profile and unsupported references/policies are refused.

The Kotlin builder exposes `unique(..., onConflict)` and
`reference(..., onDelete)`; Room exports carry unique declarations plus the
Kotlin `onConflict` overlay, and identity references with delete actions.
Reference-connected SDK tables share one deterministic reader scope, retaining
separate table grants and keys. See [the builder guide](../runtime/schema-builder.md#constraints)
and [secure SQL](../runtime/secure-sql.md#hidden-rows-and-mergeable-constraints).

The three authenticated secure tests cover all completion-gate bullets:
reversed offline clashes, both policies, loser reappearance, delete/insert races,
repointing, local statement/commit refusal, recreated parents, child-before-parent
arrival and rebuild from checkpoints/eviction. Kotlin/native qualification adds
four hand/import descriptor and fingerprint pairs, install/reopen and trusted
refusal. The Room-generated constraint fixture exercises both unique policies
and delete actions through real DAOs on Rocky's file, including whole-entity
updates of a displayed NULL reference and reopen.

Final qualification on 2026-10-08, all commands exit 0:

- `./scripts/check`: formatting, workspace Clippy with warnings denied, full
  workspace tests/doc tests, and SDK tests without default features. The three
  secure constraint gates also pass in this unfiltered run; existing unrelated
  ignored tests retain their baseline status.
- `cargo test -p rocky-secure --lib tests::constraints --locked`: 3 passed.
- `./sdks/kotlin/check`: Kotlin/JVM SDK contract and type-refusal fixtures pass.
- `./sdks/kotlin/check-schema`: 4 hand/import descriptor/fingerprint pairs,
  native secure install/reopen and trusted constraint refusal pass.
- `./kotlin test -m schema-qualification`: 3 passed, including the new Room
  constraint DAO and displayed-NULL update/reopen regression.
- `./kotlin test -m room-qualification`: all 10 tests pass, unfiltered.

The completed jj chain is `rvqlrylk` (descriptor/decision), `muosmknm`
(secure resolution and gates), `zutuzsku` (Kotlin/Room integration and evidence).
No bookmark was created or moved, and nothing was pushed.

This branch uses the existing unpartitioned format-2 schema. Phase 11's parallel
partition descriptor change must be reconciled on landing: identity columns gain
partition columns, unique indexes must include them and SetNull affects only
non-partition columns. Cross-scope references/puppets and horizon finalization
remain phases 14 and 15; this phase qualifies within one reader scope.
