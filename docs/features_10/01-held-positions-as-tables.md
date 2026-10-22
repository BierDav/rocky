# 1. Held positions as tables

Status: specified, not started. Owner: `rocky-secure` positions, readiness
and frontiers. Depends on nothing. Effort: about a week. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [lazy compaction](../runtime/lazy-compaction.md#finalizing),
[replica roles](../runtime/replica-roles.md#syncing-a-scope) and
[review finding 3](00-sync-performance-review.md#findings).

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
  count INTEGER NOT NULL, PRIMARY KEY(scope,author,sequence)) WITHOUT ROWID;
CREATE TABLE secure_log_ranges(
  scope BLOB NOT NULL, author BLOB NOT NULL,      -- signing public key
  tip BLOB NOT NULL, applied INTEGER NOT NULL,    -- 1 for applied/abandoned/held
  start INTEGER NOT NULL, end INTEGER NOT NULL,
  PRIMARY KEY(scope,author,tip,applied,start)) WITHOUT ROWID;
```

`held_positions` becomes: ranges of the domain's scopes, filtered by the
same predicates it applies today, with `last` as the maximum `end` of a
ready scope's settled frames and `counted` summed from
`secure_existence_counts` over `start..=mark`. `deferred_at` reads the
frame ranges of the keyless scope. `compute_tip_frontier` reads
`secure_log_ranges` for the tips on the workspace path, applies
`frame_path.admits` per tip rather than per frame, and walks runs instead
of positions. The frontier cache stays; invalidation now costs a few rows.

## Writers

| Event | Change |
| --- | --- |
| `insert_operation` | Frame position into `pending`; log position into `applied = 0`; existence count when nonzero |
| Status to `applied`, `held`, `abandoned`, `rejected` | Frame position to the settled bucket; log position to `applied = 1` except `rejected`, which leaves the log bucket as it was |
| Void stored | Void position; log position as applied (voids count as applied in the frontier today) |
| Placement stored, replaced or pruned | Placement position added or removed |
| Prune batch, checkpoint switch, eviction, tip drop, restore, held replay base | Positions removed with their frames; a pruned prefix needs no row because `secure_logs.pruned_prefix` already starts the frontier |

Each writer is a method on the positions module taking the decoded header
fields it already has; none decodes a header again.

## Ordered slices

1. Tables, the positions module, writers at every site listed above, the
   source test that no other site writes the tables, the format bump.
2. Readiness (`held_positions`, `deferred_at`, existence sums) on the
   tables; `cfg(test)` equality against the scan; the audit oracle.
3. Frontiers on `secure_log_ranges`; the same equality and oracle.
4. Guides: lazy compaction's finalizing section and limits, replica roles'
   frontier paragraph, the formats table.

## Completion gate

- With 10,000 frames of one author in one scope and a waiting seal,
  `seal_waits` and `tip_frontier` after invalidation each take ≤ 5 ms,
  and the `cfg(test)` scan counter stays at zero outside `audit`.
- The existing secure, SDK and e2e suites pass unchanged with the equality
  check enabled, including placements, voids, eviction, tip drop,
  checkpoint import, restore and held replay bases.
- `audit` on a file whose range table was altered by a test reports
  `Corrupt`, and reopening refuses a `v32` file.
