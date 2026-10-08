# 9. Compatibility contract and the gate to resume Room

> Superseded implementation brief, retained as an optional Room acceptance
> reference. Do not implement this sequence as the current Rocky roadmap.
> Generic work and deferred adapter work are assigned in
> [features_04](../../features_04/README.md#foundations-and-old-plan-disposition).
> [V4](../../proposals/architecture-proposal-v4.md) governs future design.

Status: superseded future brief; retained optional Room requirements. Owner: runtime/SDK qualification and shopping integration.
Depends on all prerequisite phases. This phase proves foundations; the final
driver/repository conversion begins only after the applicable gates pass.

## Capture the actual ORM protocol

Build a pinned Room 2.8.4 / SQLite 2.6.2 recording fixture around the real bundled
driver. Record statement shapes, bindings, transaction nesting and result metadata
for ordinary entities/DAOs. This is test tooling, not a production write queue.
Use exported schemas/generated DAO output to establish the protocol instead of
an invented subset of SQL examples. Requalify before upgrading Room/SQLite.

Run equivalent SQL traces through the native authorized session and compare
values, schema metadata, exceptions, counts and local transaction behavior.
Internal SQL tracing must redact credentials/private values and must never be
required in an application's repository.

## Required ordinary-access matrix

| Surface | Required coverage / blocker |
| --- | --- |
| `@Entity` / converters | Typed columns, nullability/defaults, composite/application keys and converted scalar values |
| `@Insert` / generated IDs | Single/list inserts, IGNORE/ABORT/REPLACE, returned IDs; auto-generation remains blocked until phase 1 proves it |
| `@Update` | All entity fields assigned atomically under selected changed/all-assigned policy |
| `@Delete` / writable `@Query` | Single/bulk/predicate updates/deletes, exact affected rows, authorized generated effects |
| `@Upsert` | Insert/update race, keys, incarnation and deterministic convergence |
| `@Transaction` / relations | Multi-statement reads/writes, read-your-writes, stable relation snapshot, nested savepoints |
| SELECT / raw queries / views | Aliases, joins, CTEs, aggregates, ordering, functions, binding/reset and correct result metadata |
| Reactive DAOs / Paging | Initial-subscribe race, committed invalidation, page snapshots and restart after generation changes |
| Migrations / auto-migrations | Correct logical schema/hash, index/FK validation, privileged DDL and approved replicated transforms |
| Open/close / callbacks | Connection PRAGMAs, Room identity, callback privilege classification and resource cleanup |
| Multi-domain queries/writes | Explicit bindings and independently checked grants; phase 3's atomic/read boundaries enforced |
| Partial/private/confirmed data | Declared modes, readiness errors and no fabricated NULL/empty entities |

Document FTS/virtual tables, generated columns, application triggers, custom
collations/functions, WAL/checkpoint controls, prepackaged import, destructive
fallback and multi-instance invalidation separately. Derived FTS may be maintained
from canonical ordinary tables; directly synced virtual-table writes lack capture
coverage today. Every unsupported feature must be diagnosed before mutation;
do not pretend it succeeded. Treat a feature the required application uses as
a blocking prerequisite, not permission to rewrite its repository.

Global uniqueness, arbitrary cross-file ACID and arbitrary strong Kotlin
transaction bodies remain explicit structural gates from phases 1, 3 and 7.
Accepted update publication choices do not authorize weakening those guarantees.
The final support declaration must identify the application's actual feature use
and distinguish ordinary SQLite guarantees from offline replicated semantics.

## Invariant and fault matrix

- Two replicas offline: independent edits to different fields, same-field
  collisions, same-key inserts, replacement/reinsert, delete versus stale update,
  trigger/cascade effects and reordered duplicate bundles.
- Single/list writes spanning more than 32 rows, several tables/domains and frame
  boundaries: no successful partial list, transaction or authorized subset.
- Read-only, row-restricted and field-restricted credentials; mixed denied effects;
  expiry/revocation/renewal between prepare, step, cursor fetch and commit.
- Private companions/keyless relays, dropped required bundle parts, subscribe/
  unsubscribe/eviction, key loss/install and rebootstrap without false deletion.
- Injected SQL/capture/signing/outbox/COMMIT/flush failures; process exit before
  commit, after commit before acknowledgment, and during migration/rebuild.
- Outcome lookup after reopen, compaction and fresh-node recovery; immutable
  identities/bytes, retained tombstones and custody/retention holds.
- Strong confirmation loss, takeover, missing payloads and uncertain effects;
  no coordinator/network/provider calls inside a SQL transaction.

## Scale and platforms

Preserve [existing scale budgets](../features_02/README.md#target-budgets); record
their pending baseline gates rather than declaring a documentation change passed
them. Measure schema open/install, point/filtered joins, writes/capture, observers,
cursor memory/WAL pinning and noisy-principal/long-lease fairness at relevant sizes.
Capture and invalidation must not copy whole domains or scan all observers for
each row. Report cold/warm connection setup separately.

Exercise many domains/files without requiring an attachment per file. Qualify
same-file atomic groups separately from federation/distributed decisions. Include
authenticated batches and subset visibility, not only trusted shopping writes.

Run `./scripts/check`, native SDK checks, Kotlin/core checks and the integration
fixture. After the driver exists run the full shopping check and ordinary Room
regression matrix. Hardware power-loss, remote custody, mobile backgrounding and
platform packaging need their own evidence; loopback/process-exit tests do not
qualify them. Pin supported platforms and API versions in the outcome report.

## Gate to resume the production driver

All applicable phase gates pass with evidence and selected design decisions.
Unresolved structural capabilities are listed against actual application needs;
none can be required by shopping's normal DAOs or claimed support profile.
The runtime session must already own every write/transaction/authorization path.

Then implement the small AndroidX driver, its invalidation bridge and setup, and
rewrite shopping around ordinary DAOs. The final acceptance test is concrete:

1. `ShoppingRepository` imports Room/application types only, receives only
   `ShoppingDatabase`, and reads/writes through its ordinary DAO methods.
2. Repository behavior tests run with both `BundledSQLiteDriver` and the Rocky
   driver without changing repository code or installing mock Rocky callbacks.
3. Room database entities contain no mandatory pending-mutation/replica-metadata
   types. Domain/credentials/replication setup stays outside the repository.
4. Native replication updates ordinary DAO Flows without `refresh`/`drain`, and
   a list/multi-field transaction rolls back fully under deterministic failure.
5. Offline/restart/convergence tests use actual persisted files; the old adapter
   remains supported until migration and replacement evidence pass.

If these cannot be met, stop before adding repository-specific exceptions. Append
the exact missing contract, failing scenario and evidence to the owning brief.
