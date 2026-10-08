# 5. Authorized queries, availability and observation

> Superseded implementation brief, retained as an optional Room acceptance
> reference. Do not implement this sequence as the current Rocky roadmap.
> Generic work and deferred adapter work are assigned in
> [features_04](../../features_04/README.md#foundations-and-old-plan-disposition).
> [V4](../../proposals/architecture-proposal-v4.md) governs future design.

Status: superseded future brief; retained optional Room requirements. Owner: `rocky-runtime` read planner/cursors/observation and
secure materialization. Depends on phases 1–4.

## SQL execution

Execute logical SQL through SQLite over indexed typed projections. Compile a
binding-aware plan that resolves table positions, aliases, CTEs, nested queries,
quoted identifiers, correlated expressions, views and writable targets. Do not
extend the existing token mapper by global replacement: a column, alias, string
literal and table may share a spelling. Reject unsupported constructs before
side effects. Preserve bound parameters and result column names/types/order.

Build a logical catalog from the verified installed schema. Catalog queries and
PRAGMAs must report correct tables, indexes, foreign keys and stored view SQL,
without exposing protocol objects or inaccessible domains. They may require
dedicated catalog planning; returning whatever Room expects is not validation.

Query plans use only the principal's authorized row/column projections.
Authorization predicates must constrain every access path, including joins,
aggregates, subqueries, sorting and RETURNING. Denied columns produce permission
errors, not redacted NULLs in otherwise complete entities. Row-filtered views may
legitimately exclude unauthorized rows; distinguish that authorized view from
a missing key or missing replication coverage.

Cache resolved plans by definition, binding, policy/credential generation,
projection mode and schema generation. Recheck execution authority; do not cache
grants or results across principals. Consider predicate/result side channels in
constraints, error messages, EXPLAIN and timing when defining the API threat model.

## Snapshots and cursor lifetime

One statement cursor uses one committed snapshot; multi-query relation loading
inside a transaction uses its transaction snapshot. Native cursor handles stream
bounded batches without rebuilding a result against later revisions. The current
SDK's revision-checked paginated snapshots cannot directly stand in for a SQLite
statement that advances through one consistent result.

Cursor/statement limits must include bytes, rows per batch, outstanding handles,
execution deadlines and snapshot lifetime. Reset, rebind and close release native
resources deterministically. Long reads must not pin WAL indefinitely. Revoke
authority and discard undelivered batches when required scopes/credentials change;
never continue using a stale already-authorized buffer.

For multiple files, choose and qualify a query contract: co-located execution,
coordinated snapshot leases or bounded federation with explicit consistency.
An arbitrary sequence of per-file reads is not one SQLite snapshot. Do not copy
whole domains into an in-memory database to make arbitrary SQL appear supported.
Until resolved, cross-file joins and transactional relation loads are blocked.

## Partial data and read modes

Track required resources separately from global network freshness. A locally
complete subscribed scope is not proof that every peer's newest write arrived.
The ordinary DAO mode is the authorized local materialized view; setup/status
defines that meaning without adding metadata fields to every entity.

| Condition | SQL behavior |
| --- | --- |
| Authorized scope held, required key and locally valid projection available | Execute local query, even offline |
| Authorized row selection excludes a row | Row is absent from this principal's view |
| Required scope never held, unsubscribed, evicted or being rebootstrapped | Availability error |
| Required decryption key missing or generation incompatible | Availability error |
| Pending inputs make the required projection incomplete under its declared mode | Withhold affected query; report availability error |
| Proven tombstone / authorized complete absence | Normal SQL absence |

Generated bindings declare required companions/resources so an INNER/LEFT JOIN
does not hide unavailability as a business deletion or a nullable value. A LEFT
JOIN may return NULL for an actually absent authorized companion; it must not
stand in for a missing key. SQL's optimizer must not bypass declared readiness.

Remote reads retain provenance, source revision/frontier and completeness.
Keep them outside local transaction/DAO execution unless a separately specified
mode qualifies them. Local CRUD cannot silently fetch the network while holding
writer leases. Strong reads remain explicit setup/access modes in phase 7.

## Observation and future Room invalidation

Publish logical changed-table/resource events after local commit, including
incoming sync, checkpoints, rebuild swaps and confirmed result application.
Subscribe before taking the initial snapshot so no intervening commit is lost.
Notifications may coalesce, but the next query must observe at least the indicated
committed revision. Rollbacks and provisional rows emit no committed event.

Bridge events into Room's actual invalidation protocol later. Its default TEMP
modification log/triggers run on the Room writer connection; writes on the runtime
worker do not automatically fire those triggers. Specify how logical table IDs
are registered, marked and reset, and how the Room writer connection consumes
external events without acquiring a lease recursively. A refresh call alone is
insufficient; see [the pinned source findings](research.md#exact-room-version-inspected).

Authority/readiness changes also wake readers. On logout, key loss, eviction or
rebind, terminate/revalidate subscriptions rather than delivering old data as a
fresh success. Data already held by application code cannot be erased by a Flow.

## Completion gate

- Pinned Room query shapes, joined relations, aggregates, nulls, aliases/CTEs and
  ordinary functions return native SQLite values with authorized index use.
- Adversarial SQL cannot name protocol/other-tenant tables or read denied fields
  through predicates, functions, views, catalogs, EXPLAIN or cached plans.
- Cursors retain snapshots through concurrent writes, reset/close/cancel cleanly,
  and fail safely on revocation/key loss.
- Sync, rebuild and confirmed commits produce logical invalidation without
  refresh calls, full-cache replacement or missed registration races.
- Partial/keyless/evicted tests distinguish unavailability from genuine empty
  results, tombstones and permitted LEFT JOIN nulls.
- Existing scale budgets are measured at warm/cold setup and relevant row/table
  sizes; capture cost is proportional to changed data, not the whole projection.

Run targeted read/secure/observer tests, scaling probes and `./scripts/check`.
Use [paged reads](../../runtime/paged-reads.md), [replica roles](../../runtime/replica-roles.md)
and [scale qualification](../../runtime/scale-qualification.md) for existing limits.
