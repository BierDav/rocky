# 0041: Partition constraint state and catalog verification

Status: accepted.
Date: 2026-10-09.

## Context

[Phase 4](../features_06/04-partition-aware-constraints.md) admits unique policy
indexes on partitioned tables. An id can repeat in different partitions. The
private candidate identity index and support bookkeeping previously identified
rows by id alone. Missing engine indexes would silently remove the backstop
that keeps projected rows valid.

## Decision

Constraint identities are `(table, concrete partition scope, id)`. Candidate
primary keys retain every partition column; their identity indexes include the
partition columns plus id, once each. Seeds, caches and support bookkeeping use
the same concrete scope. The current private support table is
`secure_support(tab TEXT, scope BLOB, id INTEGER)` with non-null columns and the
composite primary key `(tab, scope, id)`.

This replaces the previous private format. There is one current shape, no
migration, legacy reader or alternate encoding. At open, constrained secure
installations compare SQLite's stored SQL to the descriptor's typed indexes,
the generated candidate tables/indexes and the current support table. A missing
or altered index or an earlier private shape refuses open; reinstall the file.

Partitioned policy indexes include every partition column and use `LatestWins`.
Partitioned `SetNull` indexes remain refused so projection cannot erase an
address. Foreign keys and partitioned support tables remain refused until their
own roadmap phases. The existing unpartitioned reference graph remains valid.

## Consequences

Rebuilding or evicting a concrete partition changes only that partition's
candidates and visible rows. Resolution uses local stored winners and never
requires another partition's key. Full installation rebuild enumerates concrete
registered scopes. Equal stamps break ties by partition scope and id.

The catalog check applies to every constrained secure installation, including
unpartitioned ones. It refuses index tampering before any persisted projection
is used. Kotlin/Room admission and the full shopping schema remain phase 7;
phase 4 exposes the native portable-definition shape. See
[mergeable constraints](../runtime/mergeable-constraints.md) for semantics and
qualification boundaries.
