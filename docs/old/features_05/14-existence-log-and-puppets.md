# 14. Existence log and puppets

Status: COMPLETE. All five slices implemented and every completion-gate bullet
qualified on 2026-10-08; see [qualification](#qualification).
[Decision 0033](../decisions/0033-existence-log-and-puppets.md) is accepted.
Owner: `rocky-secure` logs, sync and typed-table projection; `rocky-core`
descriptor. Depends on
[phase 11](11-partitioned-scopes.md) and
[phase 12](12-hidden-rows-unique-indexes-and-foreign-keys.md). Wave 3. On the
critical path; the decision record may be drafted during phase 11.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [existence log](agreed-scope.md#existence-log) and
[puppets](agreed-scope.md#puppets) of the scope and the result of phases 11
and 12. A device that holds a child but not its parent's scope must learn
that the parent was deleted, and the real foreign key in its typed table
needs a parent row.

The shopping app shares one scope per list, so no key crosses a scope there.
Cross-scope keys are for the ERP cases, and their exposure is a visible
choice of the schema author.

## Ordered slices

1. Decision record: the existence log record, the puppet and virtual row
   kinds, versioning and refusal. Drafted as
   [decision 0033](../decisions/0033-existence-log-and-puppets.md); revisit it
   against the landed records of phases 11 and 12 before slice 2.
2. Existence log: per partition of a table referenced from another scope, one
   record per row (`created` or `deleted`, the `id`, no values), readable and
   verifiable by every device of the domain without the partition key. The
   log alone decides whether a row is alive, also for holders. A device
   follows the logs of the partitions its children reference. Installation
   lists the tables that have one.
3. Puppets: a row with the parent's key and `row_kind = 1`, zero values for
   `NOT NULL` columns, its own `NOT NULL` references pointing at the table's
   virtual row (`id = 0`, `row_kind = 2`) unless the parent identity is in
   the puppet's key. Created with the first child, removed with the last,
   replaced in place when the device gains the scope. Never replicated, never
   in a checkpoint digest. A trigger rejects updates and deletes.
4. Orphans across scopes: the delete action of phase 12 derives from the
   existence log; a child may reference any alive puppet.
5. Cross-scope references in the builder and from Room's export.

## Completion gate

- A device holding items but not the parent list's scope shows the item,
  hides it when the list's existence log says deleted, and brings it back when
  the child is repointed.
- Puppets are unreadable through every statement form of phase 11's check;
  `orders LEFT JOIN partial_customers` returns NULL for a puppet's columns.
- Two devices with different held scopes agree on which orphans exist.
- A replica without the parent's key verifies an existence record's signature
  and the author's right.
- [Secure SQL](../runtime/secure-sql.md) documents the exposure of an
  existence log and when not to reference a table across scopes.

## Implemented boundary (2026-10-08)

Built on `main` with phases 11, 12 and 13 landed. Decision 0033 was reconciled
with their records and accepted before code.

- **Descriptor.** Table entry `existence_log`, omitted when false (formats 2/3
  keep their bytes), bound into the replicated shape and signed manifest.
  `existence_tables()` lists the exposure. Support tables (logged tables and,
  transitively, parents of their `NOT NULL` references) must be mergeable,
  unpartitioned and keyed by `id`; otherwise `Incompatibility::SupportRow`
  names the key. Trusted refuses. Partitioned parents stay refused, as phase
  11 left constraint resolution per unpartitioned scope.
- **Existence log.** `rocky-secure` signs format-1 records per created or
  deleted row with the table pseudonym in the authoring transaction; stores
  and verifies them keylessly (signature, chain, insert/delete right, creation
  author, per-signer sequence and predecessor); refuses conflicts and second
  creations; holds gaps as pending. Liveness comes from the log alone, also on
  holders. Native sessions exchange logs by per-signer frontier; the SDK syncs
  the logs a device follows after row sync.
- **Puppets and virtual rows.** Derived in constraint resolution from typed
  rows and existence state: puppets for alive referenced rows without a visible
  real row (including a holder's hidden or evicted parent), virtual rows for
  support rows' `NOT NULL` references, replaced in place, rebuilt after
  rebuild/eviction, never captured, replicated or digested. Generated triggers
  reject changes under a session gate.
- **Orphans across scopes.** `CASCADE`/`SET NULL` to a logged parent depend on
  its liveness alone; a child may reference any alive `id`; dead, unknown and
  virtual targets fail with SQLite's foreign-key code.
- **Reads.** The phase-11 statement check filters every support table to
  `row_kind = 0`; an unheld logged parent of a readable table is nameable
  read-only and needs `row_kind = 0`. `IN (subquery)` is now checked as its own
  query block instead of being refused.
- **Builder and Room.** Kotlin `existenceLog = true` by hand or in a Room import
  overlay; the SDK keeps child and logged parent in separate reader scopes.

Guides: [existence logs](../runtime/existence-logs.md),
[secure SQL](../runtime/secure-sql.md#references-across-reader-scopes),
[schema builder](../runtime/schema-builder.md#constraints).

## Qualification

| Gate | Evidence |
| --- | --- |
| Item shown, hidden on the list's deletion, back on repoint, on a device without the list scope | `tests::existence::items_follow_the_list_existence_log_without_the_list_scope` |
| Puppets unreadable through every statement form; `orders LEFT JOIN partial_customers` returns NULL | `tests::existence::puppets_are_unreadable_and_read_only`; `rocky-storage` `partition_sql::tests::support_rows_are_unreadable_through_every_statement_form` |
| Devices with different held scopes agree on orphans | the first test (holder and non-holder, two deletions); `tests::existence::holders_and_non_holders_agree_when_a_held_parent_is_hidden_or_evicted` |
| Keyless replica verifies signature and author right | `tests::existence::a_keyless_replica_verifies_signatures_and_author_rights` |
| Secure SQL documents the exposure and when not to reference across scopes | [secure SQL](../runtime/secure-sql.md#references-across-reader-scopes) |

Further evidence: `tests::existence::native_sessions_exchange_existence_logs`
(authenticated TCP exchange); `rocky-core`
`existence_logs_bind_the_manifest_and_refuse_nondeterministic_support_rows`;
`rocky-sdk` `references_to_an_existence_log_keep_their_reader_scopes`;
`./sdks/kotlin/check-schema` with six hand/import pairs, including the Room
export overlay `existence-hand`/`existence-import`, installed and reopened
through the native SDK. The gate tests also cover rebuild, reopen, eviction,
checkpoint digest equality and that a commit publishes only child frames.

Final commands and results are listed under [final checks](#final-checks).
No Android device run, real-network throughput or Room DAO across two devices
is claimed; the cross-scope Room path is qualified at the descriptor and
installation boundary.

### Final checks

All on 2026-10-08, from the workspace root, exit 0:

- `./scripts/check`: formatting, workspace Clippy with warnings denied, full
  workspace tests and SDK tests without default features. The secure suite
  passed 137 tests (6 existing explicit ignores), including the five
  `tests::existence` gates. Log: `local/phase14/check.log`.
- `cargo test -p rocky-secure --lib tests::existence --locked`: 5 passed.
- `./sdks/kotlin/check`, `./sdks/kotlin/check-schema` (six hand/import pairs),
  `./sdks/native/check`.
- `./kotlin test -m schema-qualification`: 4 passed;
  `./kotlin test -m room-qualification`: 11 passed.
