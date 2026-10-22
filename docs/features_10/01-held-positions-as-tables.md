# 1. Held positions as tables

Status: complete (2026-10-23); see [evidence](01-held-positions-evidence.md).
Owner: `rocky-secure` positions, readiness and frontiers. Depends on nothing. Effort: about a week. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Inspect the owning modules and their tests for current behavior, and the
[performance review](00-sync-performance-review.md) for the measured baseline.

After this phase, deciding whether a seal is ready and computing a scope's
frontier read a handful of rows. No maintenance step, status call, sync
page or frontier exchange reads a stored frame header. The header scan
remains as the audit's oracle and proves the tables right.

## Settled

- **Two coordinate systems, two tables.** Readiness works in domain
  coordinates: author number and `author_sequence` from the header
  extensions, across scopes, including voids and placements. Frontiers work
  in log coordinates: the signing public key and p2panda `seq_num`, per
  scope and tip. Each gets its own ranges.
- **Ranges, not positions.** A table row is one maximal contiguous run.
  Inserting a position adjacent to a run extends it; one that bridges two
  runs merges them; removing a position splits. A log that arrives in order
  is one row per bucket however long it is.
- **Status is a bucket, not a column to update in place.** A frame's
  position moves from the pending bucket to the settled bucket when its
  status leaves `pending`; a void is its own bucket; a placement its own.
  Readiness keeps its live predicates (scope key present, scope held) at
  query time, as today, so no authority or holding decision is stored.
- **Existence counts stay exact.** Frames that sign existence records keep
  one row per position; equivocation keeps the maximum, as today.
- **One writer module.** Every statement that inserts, settles, voids,
  places, prunes, evicts, drops or restores a position goes through
  `replica/positions.rs`. A unit test over the crate's source asserts that
  the three underlying tables are written nowhere else.
- **The scan is the oracle.** `audit` recomputes both coordinate systems
  from headers and fails `Corrupt` on any difference. Under `cfg(test)`,
  every readiness check and frontier computation also compares against
  the scan, counted like `frontier_decodes` today, so the whole existing
  suite exercises equality.
- **Format.** The tables are mandatory parts of the replica file, created
  at open; the file identity moves to `rocky.encrypted-replica.v33`. An
  earlier file is refused, never backfilled.

## Tables

```sql
CREATE TABLE secure_position_ranges(
  scope BLOB NOT NULL, author INTEGER NOT NULL,   -- domain author number
  kind INTEGER NOT NULL,     -- 0 frame, 1 void, 2 placement
  pending INTEGER NOT NULL,  -- 1 while a frame's status is 'pending', else 0
  start INTEGER NOT NULL, end INTEGER NOT NULL,
  PRIMARY KEY(scope,author,kind,pending,start)) WITHOUT ROWID;
CREATE TABLE secure_existence_counts(
  scope BLOB NOT NULL, author INTEGER NOT NULL, sequence INTEGER NOT NULL,
  count INTEGER NOT NULL, settled_count INTEGER NOT NULL,
  PRIMARY KEY(scope,author,sequence)) WITHOUT ROWID;
CREATE TABLE secure_log_ranges(
  scope BLOB NOT NULL, author BLOB NOT NULL,      -- signing public key
  tip BLOB NOT NULL, applied INTEGER NOT NULL,    -- 1 for applied/abandoned/held
  domain_author INTEGER NOT NULL, delta INTEGER NOT NULL,
  start INTEGER NOT NULL, end INTEGER NOT NULL,
  PRIMARY KEY(scope,author,tip,applied,domain_author,delta,start)) WITHOUT ROWID;
```

`secure_position_sources` retains decoded coordinates by `(hash,kind)` so bulk
status changes and deletion can update the indexes without header decoding.
Log runs include the domain author and the affine domain/log sequence offset;
`settled_count` preserves pending/key filtering when existence claims conflict.
The [evidence](01-held-positions-evidence.md#indexed-coordinates-and-transactional-writers)
records why these additions are necessary to preserve the existing contracts.

`held_positions` becomes: ranges of the domain's scopes, filtered by the
same predicates it applies today, with `last` as the maximum `end` of a
ready scope's settled frames and `counted` summed from
`secure_existence_counts` over `start..=mark`. `deferred_at` reads the
frame ranges of the keyless scope. `compute_tip_frontier` reads
`secure_log_ranges` for the tips on the workspace path, clips each affine run against
`frame_path.admits` in domain coordinates and walks runs instead of positions.
The frontier cache stays; invalidation now costs a few rows.

## Writers

| Event | Change |
| --- | --- |
| `insert_operation` | Frame position into `pending`; log position into `applied = 0`; existence count when nonzero |
| Status to `applied`, `held`, `abandoned`, `rejected` | Frame position to the settled bucket; log position to `applied = 1` except `rejected`, which leaves the log bucket as it was |
| Void stored | Void position; log position as applied (voids count as applied in the frontier today) |
| Placement stored, replaced or pruned | Placement position added or removed |
| Prune batch, checkpoint switch, eviction, tip drop, restore, held replay base | Positions removed with their frames; a pruned prefix needs no row because `secure_logs.pruned_prefix` already starts the frontier |

Inserts call the positions module with the decoded header fields they already
have. Module-owned triggers maintain status and deletion changes, including
bulk SQL, in the same transaction; none decodes a header again.

## Ordered slices

1. Tables, the positions module, writers at every site listed above, the
   source test that no other site writes the tables, the format bump.
2. Readiness (`held_positions`, `deferred_at`, existence sums) on the
   tables; `cfg(test)` equality against the scan; the audit oracle.
3. Frontiers on `secure_log_ranges`; the same equality and oracle.
4. Update phase status and evidence in this directory.

## Completion gate

- With 10,000 frames of one author in one scope and a waiting seal,
  `seal_waits` and `tip_frontier` after invalidation each take ≤ 5 ms,
  and normal-query header scans and decodes stay at zero. The always-enabled
  `cfg(test)` equality oracle has separate scan, decode and elapsed-time
  accounting; its time is excluded from this production-query gate.
- The existing secure, SDK and e2e suites pass unchanged with the equality
  check enabled, including placements, voids, eviction, tip drop,
  checkpoint import, restore and held replay bases.
- `audit` on a file whose range table was altered by a test reports
  `Corrupt`, and reopening refuses a `v32` file.
