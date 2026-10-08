# 4. ORM-neutral database queries and observation

Status: implemented 2026-10-06 as Rust APIs on both profiles; see
[decision 0024](../decisions/0024-sql-surface-and-snapshot-reads.md),
[schema definitions](../runtime/schema-definitions.md) and
[queries and observation](../runtime/queries-and-observation.md), and the
[result and limits](#result-and-limits) below. Owner: schema contracts, storage/runtime
reads and secure materialization. Depends on phase 1; secure-profile slices also
depend on phase 3. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [logical schemas](../runtime/logical-schema.md), [SQL sessions](../runtime/sql-sessions.md),
[database bindings](../runtime/database-bindings.md), [secure SQL](../runtime/secure-sql.md)
and [old read requirements](../old/features_03/05-queries-and-observation.md).
Reuse phase 1–4 implementations; do not replace the storage engine or merge profiles.

Deliver an application-owned schema descriptor, managed SQL/session API, authorized
snapshot reads and committed change events. Room generation is one importer;
generic setup cannot require Room JSON, annotations or invalidation tables.
Schema descriptors are constructible from supported host-language APIs or portable
definition data without writing/compiling Rust.

## Ordered slices

1. Provide a generic descriptor construction/validation path and keep Room schema
   generation as an optional adapter. Built-in merge policies work from the
   descriptor; custom reducers are deferred ([resolved choices](resolved-choices.md)). Document key/constraint/value
   semantics separately for trusted and secure profiles. Reconcile secure deleted-key
   reincarnation/derived-column gaps by diagnosing them before installation, not
   during remote apply ([resolved choices](resolved-choices.md)).
2. Record the SQL surface and AST-based name resolution from
   [resolved choices](resolved-choices.md) in a decision record. Cover aliases, joins, nested queries and CTEs in the
   declared surface without textual global replacement. Reject unsupported SQL
   before side effects. Do not claim universal SQLite/ORM compatibility.
3. Constrain all query paths, catalogs, predicates, functions and prepared caches
   to authorized objects. Retain whole-row reader-scope restrictions; finer read
   views must not imply that a shared key enforces column confidentiality. Recheck
   authority during execution and before delivery/commit.
4. Implement bounded snapshot cursors, stable result metadata and close/reset/
   cancellation. Define WAL pinning/deadline limits. Missing required scopes,
   companions or keys yield availability errors, including LEFT JOIN readiness;
   authorized absence remains ordinary SQL absence. Remote reads stay explicit
   and outside local writer leases. Cross-file snapshots/joins remain blocked.
5. Expose logical committed table/resource events for local writes, incoming sync,
   confirmed results, rebuild switches and authority/readiness changes. Register
   before initial snapshot; coalesced notifications retain revision guarantees.
   No full-cache copying or scanning every observer for each changed row.

Maintain atomic projection/operation/outbox/outcome/revision commits and changed
versus all-assigned update policies. Ordinary SQL never writes command-controlled
or permission-changing state. Arbitrary ORM connections cannot become unmanaged
writers. Room tracker/catalog emulation is deferred integration work.

## Completion gate

- A non-Room SQL consumer constructs a schema, commits multi-table changes with
  savepoints, queries indexed joins and reopens with the same definition.
- Trusted/secure support matrices name differences; unsupported schema/SQL forms
  fail before mutation. Denied predicates/catalogs/caches cannot leak private data.
- Concurrent writes do not alter an active cursor's snapshot; revocation/key loss
  invalidates undelivered batches and releases resources deterministically.
- Missing coverage is distinguishable from genuine empty results and permitted
  NULL companions. No hidden network call occurs inside a local transaction.
- Sync and confirmed apply wake correct observers without races or provisional
  notifications. Measure applicable [scale budgets](../old/features_02/README.md#target-budgets).

Run targeted schema/read/secure/observer tests, applicable probes and
`./scripts/check`. Update runtime guides and the supported SQL/API matrix.

## Result and limits

All five slices are implemented in `rocky-core`, `rocky-storage`, `rocky-runtime`
and `rocky-secure`; the gate tests are listed in
[queries and observation](../runtime/queries-and-observation.md#qualification).
`cargo test --release -p rocky-sdk --test queries -- --ignored --nocapture`
measures, on an Apple M5 Pro with 100,000 rows in one domain:

| Budget | Measured |
| --- | --- |
| Indexed point read under 1 ms median | 0.17–0.27 ms median for a cursor's open, page and close |
| Observer cost proportional to changed data, not observer count | Flushed write medians 5.0–9.2 ms with no observers, 6.0–8.1 ms with 1,000 observers of another table and 5.9–9.7 ms with 1,000 of the written table: within the flush's own variance |
| Bounded reads | A 100,000-row cursor scan in 13–19 ms in 256-row pages |

Not done here: the native ABI and Kotlin SDK expose none of these APIs yet
(phase 8), so a host language can write definition data but not call them
without Rust; Room's catalog and invalidation tracker are not emulated; the
load stopped at retained-history quotas above 150,000 rows without delivery
acknowledgments, so the 5,000,000-row budget is not measured through this path.
