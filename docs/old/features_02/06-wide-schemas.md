# Phase 6 — Wide schemas and domain placement

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: implemented 2026-10-05, with the performance limits in the
[outcome](#outcome-2026-10-05). Measurements use SQLite
([decision 0007](../../decisions/0007-sqlite-engine.md)). Existing automatic layouts
retain phase 2's fixed assignment; explicit placement is additive
([decision 0013](../../decisions/0013-wide-schema-placement.md)).
Context: [app runtime](../../runtime/app-runtime.md), [decision 0001](../../decisions/0001-app-owned-domains.md).

Each trusted domain has live and checkpoint copies of its application schema.
Thus 2,000 tables in 64 dense domains create 256,000 physical application tables,
before protocol tables and indexes. File density governs installation and cold
connection cost; the measured placements and practical limits are recorded in
the [evidence](../../runtime/wide-schema-evidence.md).

## Slices

1. **Measure first.** With the phase 1 wide application, record install time, open
   time, identity fingerprint cost, per-statement overhead and memory at 100, 500
   and 2,000 tables, for 1, 8 and 64 domains. Per-statement overhead includes the
   SQL name rewriting each scoped connection performs; check whether it scales
   with the table count. Record engine limits met, such as schema size or
   prepared-statement behaviour. Phase 1 already met three: a 4.8 s install, a
   stack overflow in a flat 1,000-term projection query and the 256-item manifest
   limit ([baseline](../../runtime/scaling-baseline.md#wide-schema)). Propose the
   first-install budget, which the open budget excludes, and confirm it with the
   user. Also run the [phase 5 gate workload](../../runtime/write-concurrency.md#gate-workload)
   at 500 and 2,000 tables: a WAL checkpoint's stall is the file's flush and
   grows with the file. A p99 clearly above the
   [recorded limit](05-write-concurrency.md#restated-latency-gate-accepted-2026-10-05)
   of about 100 ms is explained in the outcome.
2. **Schema bootstrap.** Create a wide schema in bounded transactions with a
   resumable marker, so an interrupted install neither leaves a half-usable file
   nor restarts from zero. SQLite DDL is transactional, so each bounded step
   commits or rolls back whole.
3. **Statement cost.** Cache rewritten and prepared statements per connection so
   steady-state cost does not depend on table count. In the engine alone SQLite
   statement latency is flat to 16,000 tables; what grows is schema parsing on
   each new connection, 13 ms and 7 MiB at 2,000 tables, and table creation,
   which is quadratic ([engine comparison](../../../benchmarks/engine-scale/RESULTS.md#per-engine)).
   Measure both in Rocky, including the read-only connections beside the writer.
4. **Manifest scale.** Manifest validation, hashing and the installed-definition
   fingerprint must stay fast at 2,000 entities and their fields and indexes.
   Keep the canonical encoding unchanged unless a decision record says otherwise.
5. **Sparse domains.** A domain should not pay for tables it never uses. Evaluate
   creating a domain's tables on first use against one shared physical table per
   logical table keyed by domain. The second changes namespace isolation from
   [decision 0001](../../decisions/0001-app-owned-domains.md); it needs a decision
   record and a proof that reads cannot cross domains.
6. **More than 64 domains.** Phase 2 records a fixed layout: domain `i` in sorted
   scope order lives in file `i % files`, and the count cannot change after
   install. Add explicit placement: a recorded assignment of domains to files
   that survives adding domains, routing by scope, and opening only the files a
   process needs.
   There is no cross-file atomicity; any workflow that spans files uses durable
   inbox/outbox steps. Several files raise buffered throughput but lower it under
   full durability (16 domains: 1,846 writes/s in one file against 680 in four),
   so placement must weigh both.
7. **Schema evolution hook.** Adding a table or index to an installed application
   currently requires a new file. Define the smallest additive change that can be
   applied in place and how it interacts with the fingerprint. Full generation
   migration remains in features 1 phase 6.
8. **Encrypted path.** Repeat the measurements for encrypted row scopes with a
   2,000-entity manifest: grant verification, key lookup and scope filtering must
   not scale with the number of entities per operation.

## Gate

- A 2,000-table application installs, opens within the budget and runs the probe
  with per-statement cost within 20% of a 10-table application.
- An interrupted install resumes or fails cleanly; it never opens half-installed.
- A process serving 256 domains across several files routes every scope correctly
  and refuses an unassigned scope.
- Isolation tests show no read or write crossing a domain under the chosen layout.
- Any engine limit met is recorded with the pinned version and its workaround.

## Out of scope

Cross-domain queries and cross-file transactions.

## Outcome 2026-10-05

The user confirmed a **10-second install budget for one 2,000-table domain**.
Larger placements report total installation separately. Three isolated Rust
1.95.0 runs installed in 7.9–8.2 seconds, reopened in 97–103 ms and used 50–51 MiB
RSS. Warm read mean rose 12% against ten tables; read p50 rose 20.8%, narrowly
missing a median interpretation of the 20% gate. Both metrics are reported in
[evidence](../../runtime/wide-schema-evidence.md), alongside cold schema parsing.

All eight slices have implementations or an explicit decision:

- Bootstrap commits at most 64 DDL statements with a durable resume cursor and
  plan binding. Process-abort and failed-step tests verify recovery and refusal
  of changed plans.
- Scoped SQL and read plans cache rewrites; table, view and observer lookups use
  maps. SQLite caches prepared statements; read authorizer changes can invalidate
  them. Cold reader startup remains proportional to schema size.
- Real manifests admit 2,000 entities and their fields, with bounded decoding
  independent of operation payload limits. Canonical encoding and installed
  fingerprint versions are unchanged.
- Sparse compiled adapters instantiate only each domain's owned tables. Lazy DDL
  and shared physical tables were evaluated and rejected in decision 0013.
- Explicit append-only placement supports up to 4,096 scopes and 256 slots,
  routes by scope and opens selected files. Tests install and expand 256 domains
  with irregular assignments and verify row isolation and unassigned refusal.
- Append-only non-unique indexes are the smallest in-place evolution. Their
  immutable commitments do not change semantic identity; edits, removals and
  uniqueness are refused.
- Encrypted replicas retain the verified definition and admission sets.
  A 2,000-entity signed manifest verifies at startup; grant checks and
  author/receive/read cost stay flat across unused entities.

Dense 64 × 2,000 installation still took about 17 minutes across 32 files;
whole-process reopen took 4.6 seconds and RSS was 1.1 GiB. This is a placement
capacity limit, not a relaxed single-domain budget. The sustained runs reached 6,587 and 6,649 local writes/s with p99 100 ms;
the 2,000-table run missed the inherited every-interval throughput gate in its
final interval (4,007/s). Shared checks and the
[sustained workload results](../../runtime/wide-schema-evidence.md#sustained-full-durability-workload)
are recorded with the evidence. Another hardware profile, dense placements
beyond those measured, and generation migration remain unqualified.
