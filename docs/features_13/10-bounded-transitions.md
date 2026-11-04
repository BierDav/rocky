# 10. Bounded transitions

Status: implementation in progress (2026-11-05), starting from `578840be`.
Replayed compaction now prepares row commitments in chunks, publishes an
immutable base selector and cleans retired snapshots in chunks. Its history,
identity and custody accounting is still unbounded. R/F/S/T/M/I/P remain on
the existing paths. This phase is not complete or latency-qualified.
See [evidence](10-bounded-transitions-evidence.md).

The explicit product direction is to improve performance while preserving useful
reads and successful writes. It authorizes revising the initial sketch, including
its approval gate. The sketch is not an implementation contract. Atomic
visibility, authority, provenance, retries, revision and postcommit delivery remain
requirements. Refused writes and partition downtime are not successful latency
samples. No unbounded fallback qualifies as bounded work.

## Current ownership and coverage

The live transition engine belongs to `rocky-secure`; SQLite/session capture
belongs to `rocky-storage`, and routine sealing to `rocky-sdk`.

| Kind | Current path and status |
| --- | --- |
| R. Receive rewind | `replay_arrival`, `rewind`, `redo`: atomic transitive closure replay in the receive transaction, not bounded. Routine sealing is a trigger, not a closure cap. |
| F. Finalization | `finalize_seal_impl`, `order_behind_seal`, `take_seal_rows`: canonical late-member replay and whole-scope snapshot capture, not bounded. Snapshot generations are now immutable. |
| S. Live schema switch | `switch_schema_now`, `finish_plan`, `rebuild_replayed`, `reshape_undo`: whole-table replacement and undo rewriting, not bounded. |
| T. Tip rebuild | `rebuild_tip_workspace`, `retry_rederive`: path-dependent replay and possibly pulled bases; not bounded. |
| M. Member migration | Existing durable migration publication remains; individual partition programs are not generally bounded. |
| I. Installation | `switch_to_staged`, `install_pulled`, rebootstrap: base/history/identity/replay installation remains atomic, not generally bounded. |
| P. Projection rebuild | `rebuild_rows` invokes frame replay over the linked closure, not a stored-row refill; not bounded. |
| C. Compaction | Immutable replayed seal rows are hashed in 64-row steps and selected without copying. Retired snapshot records are deleted in 64-record steps. Publication metadata and paths without a captured snapshot remain unbounded. |

Phase 8 already fixed the old planning gaps: replayed checkpoints exist,
`reproject` skips replayed scopes, and finalized-record changes commit with base
publication. Neither short routine tips nor the interactive gate prove a bound
on arbitrary replay closures.

## Selected implementation: immutable replay bases

Compaction has an authenticated finite source: rows captured at an already
finalized seal. Application writes after that seal do not change this source.
This avoids a growing delta queue and keeps live reads and writes available.
No waiting/read-only state is introduced for compaction preparation.

- Each captured snapshot has a persisted generation allocated by a monotonic
  counter. Re-finalization writes a new generation; it never replaces rows of
  a selected generation.
- `secure_base_transition` identifies scope, seal, snapshot generation, last
  row, incremental LtHash state and readiness. Cursor and digest commit in
  the same transaction. Each preparation transaction hashes at most
  `BASE_ROWS = 64` rows using an indexed keyset scan. Dictionary reads look up
  only ordinals referenced by that chunk, not the historical field dictionary.
- A generation change invalidates preparation. A tip-path or authority change
  while unlocked returns a conflict. A concurrent local write does neither:
  its live effects and retry/outbox records stay on the existing commit path.
- All scopes prepare before any scope publishes. Replayed bases publish in
  one domain transaction. Existing signature, frontier, identity, custody,
  revision, flush and observer ordering remains in place. Those metadata scans
  are not yet bounded; counters expose their work.
- `secure_replayed_base_source` selects `(scope, seal, generation)` in the same
  transaction as its signed checkpoint. Imported bases own rows in
  `secure_replayed_base_rows`; the `secure_replayed_base` view exposes exactly
  the selected representation. Existing rebuild, audit and export paths use it.
- Every replayed base selector, signed checkpoint and the domain's irreversible
  compaction marker commit in one transaction. A crash cannot expose a parent at
  one seal and a child at an incompatible earlier seal. A later losing seal still
  triggers the existing rebootstrap rule. The generic row-frame contract retains
  its existing per-scope publication.
- `secure_base_cleanup` durably owns retirement. A retiring current snapshot
  loses its readable header before its first row is deleted. The independent
  snapshot registry retains cleanup ownership until its last row is gone.
  Selected generations and preparation inputs are pinned. Each cleanup
  transaction deletes at most 64 data/snapshot-header records; queue bookkeeping
  adds fixed work. Cleanup yields between transactions and resumes in maintenance
  or the next compaction call.

Recovery reads the committed roots; there is no partially applied live replay to
restart. A committed preparation step resumes at its cursor. A publication
transaction exposes either the previous checkpoint and selector or both new
ones. Cleanup can resume after any committed chunk without making a partial
snapshot available as preparation input. No scratch file or durable overflow
state is needed for this kind.

One replica-format bump, `v51` → `v52`, covers these local tables, views and
snapshot generations. Older formats are rejected; there are no migrations,
legacy readers or wire-format changes.

## Alternatives and subsequent work

**Whole-table/versioned publication.** It is viable where all affected rows can
be selected by a bounded metadata change. Compaction uses versioned selection
now. General application tables also need correctly rewritten foreign keys,
indexes, session capture, readers, undo and linked-scope ownership. A rename by
itself does not establish these properties.

**Snapshot computation and bounded delta publication.** It can keep successful
local writes flowing for rebuilds if publication selects an entire prepared
component. A fixed remote input cut and an explicit local-write service policy
are required. The gate's eight starvation yields count acquisitions while a
background request waits; they do not cap commits while computation is unlocked.
No convergence argument may assume otherwise. An ever-growing changed-key set
also cannot be published by an unbounded final copy.

**Per-kind state machines.** The implementation starts with C's immutable input.
P/T are the next candidates for a staged full linked-component workspace. R/F
need a finite remote cut, stable component membership, deterministic admission
against the staged effects, and a real policy for successful local writes during
preparation. S/M/I additionally carry schema, migration or authenticated import
inputs. They must not inherit a generic disposable-source recovery rule after
any live data has changed.

Remaining work includes bounding C's covered-frontier scan, identity coverage,
ledger suffix reconstruction, custody commitment and scope/metadata fan-out;
covering the no-captured-snapshot path; then implementing the remaining kinds.
The original overflow proposal is discarded: it changed live rows under a
`preparing` marker and could double-apply non-idempotent replay after restart.
Any future multi-step publication must retain its full source/output and durable
publication cursor, or use an atomic generation selector like this implementation.

## Verification and completion

Correctness checks use deterministic pauses, crash points and boundary-sized
fixtures. They cover successful writes during preparation, cursor/digest rollback
and resume, checkpoint/selector publication, cleanup, unchanged-base retirement,
same-seal re-finalization and linked-scope atomic publication.

Measure production-path lock work and release latency against `578840be` on the
same host. Keep correctness tests separate from ignored timing probes. Record
all unbounded work and limitations, including unsuccessful writes, load, sample
counts and device coverage. No phone qualification is inferred from desktop data.
The original desktop/phone hold targets remain goals, not results:
p99/max 10/50 ms desktop and 25/100 ms phone.

Completion still requires every live kind to have bounded preparation,
publication and cleanup, appropriate authority/interleaving and crash tests,
convergence evidence, paired workload qualification and passing
`./scripts/lint` and `./scripts/check`. This partial implementation does not
satisfy that gate.
