# Sync performance review

Status: findings, 2026-10-22. This records what was measured and read; the
phases of [features_10](README.md) are the response. Numbers are one run on
the reference machine (Apple M5 Pro) and the replica files it left behind;
they are measurements, not service levels.

## Measurements

The 100,000-row migration fixture (`rocky-sdk::migrations`,
`a_migration_of_100000_rows_converges_on_every_member`) on 2026-10-19
(phase 17 timing):

| Stage | Time | Rate |
| --- | --- | --- |
| Publisher fill (local authoring of the migration bundles) | 125 s | 800 rows/s |
| Member pull (one sync, frames only) | 233 s | 430 rows/s |

`dbstat` of the member's file after the run, 100,000 rows of about 50 bytes:

| Table | Size | Per row | Holds |
| --- | ---: | ---: | --- |
| `items` | 5 MB | 50 B | The application rows |
| `secure_operations` | 557 MB | 3.1 KB | 5,761 frames, average body 99 KB, average header 756 B |
| `secure_rows` | 196 MB | 1.35 KB | Materialized records |
| `secure_row_keys` and its index | 75 MB | 0.75 KB | Key per row, twice |
| `secure_admission_entries` | 14 MB | 0.14 KB | 3,125 signed receipts of 4 KB |
| Whole file | 1.0 GB | 10 KB | |

One stored record decodes to a signed creation proof (version, 32-byte
manifest, 32-byte table, id, stamp with 32-byte node key, 32-byte bindings
digest, 64-byte signature), a `fields` map keyed by 32-byte field
commitments, a `stamps` map keyed by the same commitments each holding a
32-byte node key and a sequence, then creation, identity, key and created
stamp. For seven fields that is about 1.3 KB of identifiers around 50
bytes of values. Frame bodies carry the same shapes as `RowWrite` images
plus the proof, encrypted. Syncing 5 MB of rows moved 570 MB and wrote 1 GB.

## Findings

1. **The fast catch-up path is unreachable in practice.** Checkpoint
   import is page based, 256 signed rows per page, verified off the
   replica thread; earlier evidence put it near 6,000 rows/s against 205
   to 430 rows/s for frames. A checkpoint exists only at a seal, after an
   explicit `compact`, and only when finalization is not blocked. In the
   migration workload the successor tip is open, so all rows travel as
   frames. Checkpoint instead of history
   described the symptom; the cause is that the design makes states rare.
2. **Finalizing a seal costs two full rebuilds per changed scope**
   (`crates/rocky-secure/src/replica/compaction.rs`, `finalize_seal`):
   restore rows at the frontier, reproject, compute drops, restore and
   reproject again with every operation, in one transaction, blocked while
   any author's frame through the frontier is missing in any held scope.
   `compact` replays a third time into the base. Sealing is cheap as
   metadata and expensive for every follower, which discourages seals,
   starves checkpoints and forces frame replay.
3. **Readiness and frontiers are recomputed by header scans.**
   `held_positions` reads every header of `secure_operations`,
   `secure_voids` and `secure_placements` on every maintenance step while a
   seal waits, so after every group commit. `compute_tip_frontier` reads
   every header of the scope whenever a write invalidated its cache, so
   after every received page. The parse memo saves decoding, not visits.
   Both are linear per step and quadratic over a sync.
4. **A frame is verified and opened several times.** The chain is verified
   in `receive_step` and again in `apply_frame`; the body is decrypted for
   a format check at receipt, in `dependencies_ready` once a base exists,
   to apply, and decoded again in `write_materialized`. Every stage runs
   under the replica mutex, so crypto never overlaps storage. Rounds 31 to
   46 of phase 17 removed single duplicates for about 10% each, which says
   the remaining cost is storage.
5. **Applying one row is wide**: `secure_rows`, `secure_row_keys`, the
   typed table with a SELECT before each upsert, the shadow table when
   present, identity marks, the ledger, the LtHash digest; per frame a
   signed receipt, `secure_applied`, `secure_logs`, scope state and the
   revision. Ten to fifteen statements per row is the floor.
6. **Pruning cannot help a joiner.** It needs a verified checkpoint, a
   policy, acknowledged copies and an application call; migration frames
   are exempt from the 64 MiB budget by design.
7. **Transport is serialized per page**: 32 frames, receive, flush, a
   separate acknowledgment round trip, then the next request; nothing is
   prefetched while a page applies.

## What this roadmap does and does not take up

Phases 1 to 4 answer findings 3, 1 and 2, 2, and the representation half
of 5. Finding 4 (one verified pass with crypto outside the lock) and the
statement count of finding 5 are deferred until a sampled profile of the
member stage on the phase 4 format shows their share; finding 7 is a link
protocol change deferred for the same reason and because the in-process
fixture does not measure it. Neither is forgotten: both stay listed in
optimizations on demand once this
roadmap is accepted.

## Reproduce

```sh
cargo nextest run -p rocky-sdk --test migrations --profile e2e \
  -E 'test(=a_migration_of_100000_rows_converges_on_every_member)' --no-capture
sqlite3 <member>/rocky.sqlite \
  "SELECT name, sum(pgsize)/1048576.0 FROM dbstat GROUP BY name ORDER BY 2 DESC LIMIT 12"
```

The fixture prints `STAGE` and `MEASURED` lines; the replica files remain
under the temporary directory until the process exits, so copy them from
a paused run or add the `dbstat` query to the fixture's `ACCOUNT` output.
