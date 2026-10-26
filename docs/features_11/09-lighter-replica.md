# 9. A lighter replica

Status: incomplete. The official 100k migration now passes its correctness
assertions, and a physical iPhone LAN load and offline restart passed.
The strict single-row receive statement gate remains open at 83 statements;
the isolated issuer-time qualification is deferred at the owner's request.
Final correctness checks pass all assertions, but the integration suite
exceeds its unchanged 300 s budget (336.89 s), so full check is not green.
See the [evidence](09-lighter-replica-evidence.md). Owner: `rocky-secure` row records, identity, checkpoints,
placements and held refresh; `rocky-storage` connection settings. Depends on
[phase 4](04-recovery-that-drives-itself.md), so the replica format moves
once after it. Effort: two to three weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [review](00-reliability-review.md), findings 10 and 11. The 100k
member file is 161.7 MB for 5.1 MB of rows: row keys and their index take
76.5 MB, checkpoint rows repeat materialized rows, and creation headers
fill pages at 40%. Each applied row reads its record twice, deletes from
`secure_move_rows` unconditionally and decodes the record about five times.
Placement serving and held refresh scan history per request.

Start with the member and issuer `dbstat` tables and a statement count per
applied row, recorded in evidence. After this phase rows are stored once,
keys compactly, and apply does only the statements a row needs.

## Settled

- **Base and overlay.** A row equal to its base record is stored once, in
  the base. `secure_rows` holds a record only where it differs from the
  base, including tombstones; reads resolve through one view that prefers
  `secure_rows`. Rebuild, finalization, audit and transfer read the same
  view. The LtHash digest and every audit equality stay byte-identical.
- **Compact keys.** `secure_row_keys` uses the interned integer scope id;
  its lookup index holds a 16-byte key digest, and lookups confirm the full
  key from the row.
- **Fitting tables.** `secure_creation_headers` becomes a rowid table with
  a unique digest index. Checkpoint tables keyed by scope and row become
  `WITHOUT ROWID`. Checkpoint pages insert in key order.
- **Fewer statements per row.** Apply reuses the record it read, deletes
  from `secure_move_rows` only when the old record moved, decodes once,
  merges the winner and seal-slot reads, and signs the admission head once
  per transaction.
- **SQLite settings.** Statement cache 512; the writer's configured main
  and temporary page-cache budgets total 32 MiB on desktops and 8 MiB on
  Android, split equally. `temp_store` stays at its default; the measured
  reasons are in the evidence. Reader `mmap_size` is 64 MiB;
  passive WAL checkpoints at 32 MiB with `journal_size_limit`;
  truncation waits a bounded time for readers. New files use
  `auto_vacuum=INCREMENTAL`, and compaction and pruning reclaim free pages
  in bounded steps. `PRAGMA optimize` runs at close.
- **Indexed scans.** Placement serving looks up sources by author and
  sequence range. Held refresh filters by scope and runs only when the
  graph, keys or grants generation changed. A partial index on pending
  heads makes the apply query covering.
- **Format.** The replica moves one version. Row, checkpoint and link
  formats are unchanged.

## Ordered slices

1. `dbstat`, statement counts and profiles; evidence baseline.
2. Base and overlay with audit equality.
3. Compact keys and fitting tables.
4. Per-row statement cuts.
5. SQLite settings and space reclamation.
6. Indexed placement serving, scoped held refresh, pending-heads index.
7. Evidence.

## Completion gate

- The 100k member file is at or below 80 MB.
- The issuer's 100k author and publish takes at or below 70 s and its file
  is at or below 300 MB.
- Counting hooks show at most eight SQL statements per applied row for an
  ordinary update.
- Full-rebuild, finalization and audit equality tests pass unchanged; the
  LtHash digest of the 100k fixture equals its phase-start value.
- Placement requests on a 100k history read no operation bodies.
