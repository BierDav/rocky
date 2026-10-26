# Phase 9 evidence

Status: partially implemented (2026-10-26). Both file-size gates and the
statement gate for batched updates are met on the 100k fixture; the issuer
time gate is not. Measured on base `623ef5e2` in the isolated Jujutsu
workspace `lighter-replica`, then rebased onto phase 4 (`d1d5eb13`), which
landed meanwhile. The replica identity moves to **39**, after phase 4's 38.

## Implementation

- **Shared base.** `secure_checkpoint_base` replaces the base row table, as
  `WITHOUT ROWID`. Its `fields` is NULL while the stored row holds the same
  record. `secure_checkpoint_rows` is now a view that resolves the record;
  inserting through it shares equal rows. Updating or deleting a stored row
  first copies a shared base record back into the base, so the base never
  changes underneath it. Bulk rewrites (rebuild, rewriting import) share the
  base again afterwards. Readers, audit, transfer and finalization read the
  same view; signed bytes and digests are unchanged.

  This keeps `secure_rows` holding every current row, rather than holding
  only the rows that differ from the base as the brief settled. The
  orientation was flipped because `secure_rows` carries the shape, seal
  candidate, kept-history and key triggers and is read directly in about 30
  places; the stored bytes saved are the same.
- **Compact keys.** `secure_row_keys` is keyed by the interned scope id and
  holds a 16-byte key digest. Every lookup confirms the full key from the
  decoded row.
- **Fitting tables.** `secure_creation_headers` is a rowid table with a
  unique digest index. Checkpoint overlay, scratch, rebootstrap and served
  rows are `WITHOUT ROWID`.
- **Fewer statements.** Apply reuses the record it read for the digest
  update, key index and projection; it deletes from `secure_move_rows` only
  when the replaced record was a marked move. Scope-id lookups are cached
  with the record dictionary, which every rollback clears.
- **Replay.** A finalization replay rebuilt the identity ledger from the base
  for every frame (quadratic in frames). Births only accumulate and merges
  are order-independent, so the ledger is extended from the previous step,
  within one transaction. Frame paths depend only on the frame's tip and are
  computed once per tip per step; signer keys are decompressed once.
- **SQLite settings.** Statement cache 512 on every connection; readers map
  64 MiB. `temp_store` stays at SQLite's default: with `MEMORY`, every
  statement end truncated an in-memory statement journal by walking it, which
  made publication 3× slower (`memjrnlTruncate` led its profile). Writers use a 32 MiB page cache (8 MiB on
  Android). The replica checkpoints passively at 32 MiB, cuts the WAL back
  to 32 MiB, and a truncating checkpoint waits up to 50 ms for readers. New
  files use `auto_vacuum=INCREMENTAL`; after each commit, at most 4096 free
  pages are returned once 1024 have collected. `PRAGMA optimize` runs at
  close.
- **Indexed scans.** Placement serving reads headers through
  `secure_position_sources` by author and sequence, and only the length of
  an operation body. A partial index covers the pending log heads.

## 100k fixture

`cargo test -p rocky-sdk --test migrations --release` then
`a_migration_of_100000_rows_converges_on_every_member --exact --nocapture`.
The test now also prints the issuer's `dbstat` and both files' free pages.
The baseline is the base commit in a second workspace, run concurrently with
the optimized build so both saw the same host load (18 cores, load 20–90
from other workspaces). Times are therefore paired observations, not
isolated benchmarks.

| Measure | Base | This phase | Gate |
| --- | --- | --- | --- |
| Member file (main) | 166,494,208 B | 67,411,968 B | ≤ 80 MB, met |
| Issuer file | 719,355,904 B (123,293 free pages) | 125,304,832 B (976 free) | ≤ 300 MB, met |
| Issuer fill (open running both migrations) | 735.9 s | 239.6 s | |
| Publication to one peer | 74.4 s | 85.8 s | |
| Closing compaction | 32.3 s | 37.7 s | |
| Member pull | 13.6 s | 15.2 s | |

An earlier pair at heavier load gave 168.6 → 69.0 MB, 719.3 → 115.5 MB and
a fill of 954.7 → 262.9 s. Publication and the member pull are within the
spread of these runs; the fill is not.

Member tables (bytes on disk):

| Table | Base | This phase |
| --- | --- | --- |
| `secure_row_keys` + index | 76,697,600 | 12,517,376 |
| base rows (+ autoindex) | 34,033,664 | 4,509,696 |
| `secure_creation_headers` | 14,659,584 | 6,422,528 |
| `secure_rows` | 29,413,376 | 29,413,376 |

Row and checkpoint encodings are unchanged, the closing checkpoint verified
on both devices and the member took it in one transfer, and finalization
and rebuild equality tests pass. The fixture's LtHash digest was not compared
with a phase-start value: the fixture signs fresh keys each run.

## Statements per applied row

`tests::scaling::an_ordinary_update_applies_in_few_statements_per_row` counts
statements prepared on the applying thread (`rocky_storage::sqlite::statements`,
test support only).

| Frame | Base | This phase |
| --- | --- | --- |
| 32 updated rows | 8.66 per row | 6.34 per row |
| 1 updated row | 94 | 82 |

The gate holds for batched updates. A single-row frame still runs about 80
statements, mostly per-frame reads of subscriptions (11), the roles
configuration (10) and the newest checkpoint (8); caching those is left open.

## Not done

- Held refresh still scans applied and pending operations and runs on every
  request, not only after a graph, key or grant generation change.
- The issuer's author and publish (fill plus publication, 325 s here) is far
  above 70 s on this host. Before this phase, finalization replay led the fill
  profile: frame-path walks re-encoding seals, Ed25519 key decompression and
  the per-frame ledger query. After it, the fill is led by `pwrite` from
  `pagerStress`: the migration's single transaction outgrows the 32 MiB page
  cache and SQLite spills pages to the WAL mid-transaction.
- `Checkpoint pages insert in key order` was not changed.
- `./scripts/check` passes all 200 unit and 1,404 integration tests but is
  not green on time budgets. Before the rebase,
  `tips races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner`
  took 2.20–2.24 s alone (the base took 2.12–2.29 s on this host, as phase 6
  also recorded). After it, `live_links::iroh_tie_gate` took 4.13 s alone;
  reruns took 1.12, 1.17 and 4.12 s, a network-timing split this phase
  does not touch. Both budgets are 2 s.
- Physical-device runs were not part of this phase's gates and were not done.
