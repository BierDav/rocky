# Wide schemas and domain placement

Trusted `Runtime` installation of many tables and domains, explicit placement
and additive indexes. Measurements: [scaling limits](scaling-limits.md#measured-limits).

## Installation and statement cost

`Runtime` installs base protocol, application and checkpoint-copy schemas in
transactions of at most 64 statements. A private marker stores the binding,
plan commitment and next statement. Each step commits or rolls back its DDL
with its cursor. The final transaction publishes the file binding and removes
the marker. Admission begins after all selected files finish.

An interrupted open must use the same pending plan. A different schema,
identity, author configuration or pending domain set is refused. Already
committed steps are retained; the runtime never infers progress from partial
application tables. A single-domain `Replica` file whose schema has at most 64
statements installs in one transaction; a larger one uses the same bounded
mechanism.

Each scoped connection keeps a bounded rewritten-SQL cache. Logical-name
lookups, authorized-view lookup and source-to-observer routing use hash maps.
Read plans also cache rewritten queries, bound to the installed caller's views.
SQLite caches prepared statements on each connection. Neither cache stores query
results or grants. Cold read connections parse the file's schema during connection
setup, before starting a bounded application snapshot. This cost remains visible
in first-read latency and outside the 500 ms read deadline, which a 2,000-table
schema on slow storage would otherwise exhaust.

## Explicit placement

Use `Runtime::open_placed(path, installations, placement, options)`. Each
`DomainPlacement { scope, file }` assigns a scope to a zero-based file slot.
`RuntimeOptions::files` fixes the slot count at first install, up to 256 with
explicit placement. The runtime supports up to 4,096 installed or assigned
domains and installs at most four files concurrently.

The catalog is `path` with `-placement.sqlite` appended. Slot 0 is `path`, and
slot `k` follows the existing sibling naming convention, for example
`laptop-3.db`. The catalog is metadata only; a process may open slot 3 without
opening slot 0. It opens only files containing its selected installations and
runs one writer per selected file. Processes serving disjoint files can coexist;
file ownership still excludes two writers of the same file.

Pass the complete recorded assignment set plus additions on each open.
Installations may select a subset of those scopes. Adding scopes leaves existing
assignments and identities untouched. A recorded assignment moves only through
an offline [placement migration](database-bindings.md#move-domains-between-file-slots).
Moving or removing a recorded assignment at open,
changing the slot count, opening an unassigned scope or changing an installed
domain's identity is refused. A scope not selected by this process is unavailable
through `Runtime::domain`, even if it is assigned in the catalog.

An installed domain's manifest remains part of its identity. Predeclare future
domain rules, or supply a compiled adapter whose manifest binds only that domain's
rule. The latter permits new domain IDs without changing any existing manifest;
the runtime does not weaken fingerprints to accept edited definitions.

Placement metadata is flushed before file creation. If a process dies after a
file completes but before catalog completion, reopening verifies the installed
identity and completes the catalog entry without replaying DDL. A missing
completed domain or foreign file fails closed. Keep the catalog and all selected
files together when backing up or restoring. There is no cross-file transaction;
workflows spanning files require durable inbox/outbox steps. Domains that need
one SQL transaction share a slot through a [database binding](database-bindings.md).

`Runtime::open_with_options` uses the
[automatic layout](write-concurrency.md#file-layout): sorted scope order modulo
files, at most 16 files. Changing its installation set requires a new layout,
and a layout cannot be converted to explicit placement in place.

## Sparse domains

The manifest declares logical application entities; `schema()` declares the
physical storage of an installed domain. An application may instantiate a
scope-specific adapter with only the tables that domain owns. Its authorized
views and checkpoint table list must match that schema. The runtime binds this
choice in the domain's fingerprint; changing its base tables later needs a new
[generation](generations.md).

This keeps the domain's namespace isolation and avoids creating tables that
adapter never uses. The runtime does not infer usage or create declared tables
on first read.

Count both live and checkpoint copies when placing domains. At 2,000 application
tables, two dense domains already create about 8,000 physical tables before
protocol tables and indexes. A file's practical capacity and first-install cost
must guide assignments alongside durability and throughput.

## Additive indexes

Override `CompiledSchema::additive_indexes()` with static `CREATE INDEX`
statements on existing application tables. Use stable index names, no uniqueness
and no extra statements. The runtime namespaces names, installs DDL and records
immutable commitments in the domain's transaction before admission. Entries may
only be appended. Repeated opens are idempotent; edits or removal are refused.

Reducers and their SQL must remain deterministic regardless of scan order;
queries requiring order must declare it.

These indexes do not change `rocky_core::installed_definition`, the semantic
fingerprint used by operation identity and checkpoints. The base schema and
manifest remain unchanged. Indexes are installed on live tables; checkpoint
copies keep their base schema. A new table, field, constraint, reducer or
projection generation is outside this hook.

## Manifests and encrypted scopes

Manifest validation and decoding accept up to 32,768 items per group, 4,096
domains and a 4 MiB body, independent of operation payload limits. Canonical
encoding is sorted by definition IDs. `installed_definition` exposes the
compiled fingerprint, including base DDL and handlers.

`EncryptedReplica` verifies its signed manifest at open, then retains the
verified definition and hash sets of declared tables, fields and domains.
Per-operation admission uses those sets. Credentials remain verified against
current epoch, revocations, time and subject on each admission. Grant and key
work depends on granted scopes, with the 64-grant bound, rather than on unused
manifest entities. Signed-manifest decoding has its own input bound; encrypted
operation and transport frames keep their 256 KiB limit.

## Design rationale

- **Append-only placement catalog, not a wider automatic layout.** Sorted
  round-robin cannot add a domain without moving existing assignments and
  changing the aggregate binding.
- **Bounded bootstrap steps.** SQLite DDL grows quadratically with the tables
  of a file, so one transaction for a wide schema is unbounded; no domain is
  admitted until its file's bootstrap completes. The budget is 10 s for the
  first install of one 2,000-table domain; larger placements report their own
  time.
- **Eager schema creation, no lazy DDL.** Bound reads use read-only
  connections, and checkpoint enumeration and read plans need the installed
  tables; first-use creation would need writer admission for reads.
- **No shared physical tables keyed by domain.** That would change the
  isolation contract and the SQL authorization boundary.
- **Indexes are the only additive change.** They sit outside the semantic
  fingerprint; tables, fields, constraints and reducers change through a
  generation.
