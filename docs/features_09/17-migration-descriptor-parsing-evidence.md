# Stored migration descriptor parsing

Status: bounded round39 implementation and one comparison complete (2026-10-20);
final qualification and whole-change acceptance remain pending. The [timing record](17-schema-changes-through-seals-performance.md)
retains the failed combined 900 s gate and earlier measurements.

## Boundary

Only the signed BLOB decode in `Inner::migration_membership` uses this private
memo. Each replica Inner retains at most one successful
`SignedMigrationPublication::from_bytes` result and its complete input bytes.
The retained raw input has a 32 KiB cap; the parsed object, return clones and
allocator overhead make total heap use larger. Alternating descriptors can
replace the slot on every call. Oversized input goes directly to the original
parser before hashing or copying; memory pressure is never a new wire quota.

A hit requires Hash of the entire freshly selected BLOB and complete byte
equality. Returned descriptors are owned clones. Errors are never retained.
The fact may survive transactions and rollback because canonical bounded decode
and statement validation depend only on those exact bytes. Close drops it;
reopen starts empty. No evidence or cache flag is persisted.

Every membership query, row-shape/conflict/NULL/type check, publication length
conversion and proof decode keeps its existing order. The memo grants no
membership, custody, Applied status, readiness or current authority. Actual
signature, opening, time, revocation, schema, graph, finality and proof checks
remain at their existing consumer boundaries. Historical audit keeps its
separate historical contract. `migration_record`, audit, network decoding and
the public parser remain unchanged.

## Focused controls

The small SQLite controls under `tests::bulk` exercise actual stored-frame
reconstruction and actual migration-frame checking, with private test-only
parse/hit counts. They cover owned clone isolation, cold reopen, fresh SQL and
precise first errors, canonical changed values, uncached malformed/noncanonical/
over-quota input, statement validation, changed proof index/root, genuine issuer
expiry, current revocation/minimum changes, bad signatures, commit failure and
retry, and disabled/tiny/one-entry/raw-byte fallback. Privileged retained-store
mutations are rolled back before ordinary audit. The original nine bulk cases
retain their assertions, including the distinct post-commit flush failure.

The external production build has no counters; small actual-consumer counts
establish reuse, rather than inferring it from elapsed time. They do not measure
an external end-to-end hit rate or time saved by parsing alone.

On final code, the secure selection passes 27/27 in 2.562 s, slowest0.183 s.
It includes all nine original bulk cases, nine new controls and nine retained
claim/replay/signature/restoration/kept-byte controls. The owned reconstruction
control records (full parses,hits,entries,raw bytes) = (1,2,1,1072); reopening
performs a new cold parse during actual tail reconstruction. All new cases
remain below the unchanged 2 s integration budget. Logs and complete command
arguments are `/tmp/rocky-round39-controls-green.log` and
`/tmp/rocky-round39-commands.jsonl`. The SDK two-step migration without default
features passes in 1.030 s in its integration profile.

The real-voter private migration passes in its proper e2e profile in 4.580 s.
Scoped secure all-target Clippy passes in 12.566 s. No full matrix was run.

## One external 30k comparison

The unchanged external helper passes once in **107.25 s** on candidate snapshot
`32bdee90e6ec`, versus the current round38 baseline **120.22 s**. Seed/additive/
fill/member stages take **12.876018792 / 7.849533125 / 36.739766125 /
48.639143584 s**; baseline stages are 13.169798625 / 8.058829791 /
38.148773416 / 59.336581583 s. The observed total difference is 12.97 s;
member convergence is 10.697438 s shorter. This single observation does not
isolate parse/clone costs, prove a hit rate or predict the proper gate's savings.

One sync and all original exact-data/accounting assertions pass. The peer-closed-
link report precedes successful convergence. The fresh publisher/member
migration accounts each hold 185,842,457 B; ordinary accounts hold 10,311 / 0 B.
The baseline's fresh fixtures account 184,471,507 B of migration data each;
identical encoded byte counts across independently generated fixtures are not
claimed. The helper, manifest/profile/features and assertion code are unchanged.

Compilation is excluded (Cargo build60 s; whole driver169.283 s). The test-only
180 s watchdog starts after `running 1 test`. Two-second additive, member-early
and member-late samples are recorded; member captures start after1/15 s from
fill completion, as in the baseline driver. Logs are
`/tmp/rocky-round39-small-30000-after-1.log` and
`/tmp/rocky-round39-measurement.log`; driver is
`/tmp/rocky-round39-small-profiler.py`. No production counters were introduced.

This external harness has a different feature graph from proper qualification.
The comparison establishes an observed within-harness improvement only. No
second measurement, cache widening or full matrix follows from this experiment.
Root chooses any next unchanged proper100k gate. Combined900 still requires
both actual suites and fresh whole-change HIGH acceptance; phase20 stays pending.
If further attribution is needed, separate descriptor parse/clone, live crypto/
SQL, and verify_open replay versus recomputed-ledger payload decode on a fresh
proper run, as round39 recommends.
