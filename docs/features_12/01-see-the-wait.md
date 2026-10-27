# 1. See the wait

Status: planned. Owner: `rocky-secure` (replica lock acquisition, spans),
`rocky-sdk` (write probe and load scenarios). Depends on nothing.
Effort: one week. Follow the [execution contract](README.md#agent-execution-contract).

## Objective

Nobody has measured how long a local write waits, or what it waits
behind. This phase makes every replica lock acquisition observable,
defines the probe and the load scenarios every later phase measures
against, and records the baseline. It changes no behavior.

## Settled

- **One acquisition wrapper.** Every acquisition of the replica's `Inner`
  mutex goes through one function: `lock()`, `lock_owned()` and the
  `transaction!` macro's acquisition, about 350 sites in `rocky-secure`.
  The wrapper is a plain (non-async) `#[track_caller]` function that
  captures `Location::caller()` before returning the future. It takes a
  class, `Interactive` or `Background`. Interactive covers SQL session
  `BEGIN`, a writing autocommit, `author_bundle` and the
  `author_transaction` entry point; everything else is background. The
  guard records wait time (request to acquisition) and hold time
  (acquisition to release), with the call site and class.
- **Reporting.** Each hold over 1 ms emits a `tracing` event at debug
  level on the existing `replica` target, with fixed fields: `site`,
  `class`, `wait_us`, `hold_us`. A test-support histogram per site, under
  the existing `qualification` feature, feeds the measurement tests. No
  new dependency. In release builds without a subscriber, the wrapper
  costs two `Instant` reads per acquisition.
- **The write probe.** On an open SDK `Database`, every 100 ms the probe
  does the following:
  - Call `Database::transaction()`, run one `UPDATE` of one row in a
    mergeable table, and call `commit()`.
  - Record `write` as the time from `transaction()` being called until
    `commit()` returns.
  - Record `observed` as the time from `commit()` returning until an
    `observe_tables` notification for that table arrives.
  - Record the breakdown from the wrapper: lock wait, hold, and the
    flush wait when present.
  - Each scenario runs 600 samples (60 s) after 5 s of warm-up.
  - Report p50, p95, p99 and max.
- **Load scenarios.** Each scenario runs the probe on one node while the
  load runs.
  - **L0 idle.**
  - **L1 bulk receive.** The node pulls the 100k migration fixture from a
    peer: the member pull in `rocky-sdk::migrations`.
  - **L2 small receive.** A peer authors one-row writes at 50 per second
    and the node receives them.
  - **L3 local maintenance.** The node runs closing compaction and seal
    finalization over the 100k fixture.
  - **L4 checkpoint pressure.** A second local writer commits 32-row
    transactions continuously, so passive and truncating WAL checkpoints
    both occur.
- **Probe durability.** Run the probe twice: once as the SDK behaves today
  (`Flushed`), and once with a connection opened as `Durability::Buffered`.
  This separates flush cost from lock cost before phase 4 changes the
  default.
- **Statement counts.** Extend the existing statement-counting test
  support (`rocky_storage::sqlite::statements`, `trace_statements`) to a
  single-row local author. Record the count and the trace grouped by
  table, beside the existing single-row receive count.
- **Per-unit costs.** For each background holder that phase 2 will bound,
  record the measured hold per unit of work (frame, row, page or byte) on
  desktop and on the slowest available phone. Phase 2 derives its bounds
  from these numbers.

## Ordered slices

1. Acquisition wrapper with class and call site; migrate every site
   mechanically. No behavior change.
2. Tracing event and qualification histogram. Test that a hold above
   1 ms is reported with its site, and that classes are attributed
   correctly for the four interactive entry points.
3. Write probe and L0–L4 harness as `#[ignore]`d release tests in
   `rocky-sdk`, reusing the 100k migration fixture code. Desktop baseline.
4. Single-row author statement count and trace.
5. Phone baseline for L0 and L2 at least, using the features_11 device
   harness. Record L1, L3 and L4 on the phone where the harness supports
   them; otherwise mark them not measured.
6. Evidence: for each scenario, the probe percentiles and the top 10
   holders by p99 and by max hold, with site, class and unit of work.
   Include per-unit costs and the author statement count.

## Completion gate

- Every `Inner` acquisition in `rocky-secure` goes through the wrapper.
  A test or lint check fails on a direct `inner.lock()`, `lock_owned()`
  or bare mutex acquisition outside the wrapper.
- `./scripts/check` passes. The wrapper's overhead moves no tiered test
  over budget, and the paired single-row receive and 32-row statement
  counts are unchanged.
- The evidence contains desktop L0–L4 and phone L0 and L2 baselines in
  both durability modes, with the per-site holder table and per-unit
  costs.

## Not in scope

Changing any bound, priority, durability default or statement. If a
measurement shows a goal in the [README](README.md#goals) needs a
different phase split, report it in evidence. Don't act on it.
