# 3. Intent shapes

Status: measurements finished (2026-10-29). The reported migration and shopping
validation failures are repaired; the captured corpus remains from the original
measurement run. See [evidence](03-intent-shapes-evidence.md#shared-validation-repair-2026-10-29).
Owner: test support in `rocky-secure` and `rocky-sdk`,
plus the example applications. Depends on nothing; touches no engine
code. Effort: three to five days. Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

Measure what real write transactions look like before building the
engine. The prototype names this as its deciding input: how many
transactions are keyed, commute, collapse, depend on constraints or
would be refused by the intent rules. The result fixes phase 6's
fast-path operation set and phase 4's routine seal cadence.

## Settled

- **Sources.** Every write transaction of the shopping list app's
  qualification scenarios, the inventory, todo, employees and directory
  examples, and the SDK integration tests that model application flows
  (not the format or fault-injection tests). Record each source and its
  transaction count.
- **Capture point.** A test-only hook at SQL session commit records the
  statements, bound parameters and captured rows. It writes nothing
  persistent and is compiled only for tests.
- **Classification.** Each transaction gets exactly one class, checked in
  this order:
  1. *Refused*: would be refused by the README's determinism or
     partition rules, or split beyond 64 partitions.
  2. *Coordinated*: issued as a coordinated write.
  3. *Constrained*: touches a unique index, foreign key or CHECK.
  4. *Keyed*: every statement addresses rows by full primary key or
     inserts a fresh key.
  5. *Predicate*: uses a filter inside one partition.
  6. *Split*: reaches several partitions.

  Separately, per transaction: the statement count, whether effects
  collapse (repeated increments or assignments of one cell), and the
  algebra the prototype supports (add, assign, append, keyed delete,
  fresh insert).
- **Stop rule.** If *Refused* exceeds 10% of transactions in any source,
  stop and report the refusing constructs before phase 4 starts. Phase 4
  then needs a decision on those constructs.
- **Decisions this phase makes.**
  - Phase 6's operation set: the algebra operations present in at least
    5% of transactions across all sources. Fewer means phase 6 still runs,
    but with a smaller set.
  - The routine seal cadence keeps `ROUTINE_SEAL_TRANSACTIONS` at 1,024
    unless the measured transaction rate of the shopping app would seal
    more than once per minute of active editing. In that case raise it
    to the next power of two that doesn't. Phase 4 can still lower it from
    rewind costs.

## Ordered slices

1. Test-only capture hook and a runner that writes the classification as
   JSON into the evidence directory.
2. Run every source; record counts per class and source.
3. Record the decisions above with the numbers that drove them.

## Completion gate

- A table per source with counts per class, statement counts and the
  algebra breakdown, plus the reproduction command.
- The fast-path set and seal cadence are recorded, or the stop rule is
  reported.
- `./scripts/check` passes; the hook is absent from release builds.

## Not in scope

Real production traces (none exist), engine changes and performance
measurements.
