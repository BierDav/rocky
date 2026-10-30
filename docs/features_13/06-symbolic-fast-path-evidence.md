# 6. Symbolic fast path: evidence

Result: **not merged.** Equivalence held, but the paired advantage failed, so
under the merge rule the crate and its caller were deleted. No code from this
phase remains, no format changed, and phase 8 proceeds without it.

Measured on snapshot `aadff6ac82f4` over phase 5's `c8db1fd9`, 2026-10-30. Host: macOS 27.0.1 arm64, 18 cores, load average 5 to 15 from other
work. The deleted implementation is kept as a patch at
`local/phase06/rocky-symbolic.patch` (not tracked), with the logs beside it.

## Slice 1: algebra and lowering

`crates/rocky-symbolic` depended only on `rocky-core`. It lowered a whole intent
to keyed assign, add, append, keyed delete and fresh-key insert, or refused it.
Statements were read with a small strict tokenizer, since the crate may not
depend on a SQL parser; anything unknown was opaque. Its forms are what
`split_intent` renders: numbered parameters, qualified partition filters and a
filter repeating a key. Coordinated tables, tables with generated columns,
non-binary keys and values outside the column's own storage class were opaque.

## Slice 2: pairwise commutativity

Every pair of a matrix of bound statements ran in both orders in plain SQLite,
the independent interpreter, over four states; every proven pair matched in rows
and outcomes. `rocky-symbolic`: 10 tests passed. Three findings shaped the proof:

- Equal assignments to one cell end alike but do not commute: grants read what
  changed, so which of them needs the field right depends on order.
- Two adds to one cell commute only while no prefix can leave `i64`, which
  depends on the state. The proof returned the cell and the sum of the adds'
  magnitudes, and the caller read the cell before applying directly.
- Fresh inserts into a table with any covered column cannot commute under the
  settled rule, because an insert touches every column.

## Slice 3: caller

`replay_arrival` applied directly when the arrival was a single member, each
crossed transaction was a single member, and the pairs commuted. Crossed
transactions of linked partitions counted, and their authority was verified
again, as redo does. A test-only switch forced rewinds.

A direct apply breaks one assumption of the rewind: undo images hold the state
each transaction found, so they restore only in application order. The caller
added an `applied` column and index to `secure_replayed`; a rewind undid in
reverse application order and took in transactions applied after anything it
rewound, which can reach behind the arrival, so those redid before it. The
commuting fixture caught the first version, which redid them after the arrival.

Statement budget, asserted: one late add crossing 32 transactions spent 88
statements against 92 for an in-order receive of a same-shaped add, within
in-order + 5. Direct arrivals in the mix spent at most 91.

## Slice 4: equivalence

Each schedule ran on fresh receivers with the fast path and again with rewinds
forced, against the from-scratch canonical replay, on rows and drop map.
`cargo test -p rocky-secure --lib -- tests::replayed_constraints
tests::partitions::replayed tests::sql::intents`: 44 passed, 1 ignored
(the measurement).

| Fixture | Schedules per arm | Direct applies |
| --- | ---: | ---: |
| Phase 4: four and five transactions, seeded, successor | as in phase 4 | not counted |
| Phase 5: graph, checks, indexes, cascades, caps | 720, 120, 192, 192, 192 | 0 |
| New: commuting adds around a non-commuting doubling | 1,920 | 1,014 |

Condition 1 holds, but the phase 5 fixtures never took the fast path: all their
pairs touch covered columns or are bundles. The direct path's equivalence rests
on the new fixture and on whatever phase 4 schedules reached it. Mutation checks
of the rewind change were not run.

## Slice 5: paired measurement

Release test build, 16 late arrivals per round, 40 rounds, 640 samples per arm;
the arms alternate first. Each arrival is timed through `receive_all`.

```sh
cargo test --release -p rocky-secure --lib -- --ignored --nocapture late_arrivals_in_the_mix
```

The mix is phase 3's 137 transactions that are not coordinated: 85 constrained
(`items`, unique title and foreign key), 47 keyed (`notes`, no constraint) and
5 predicates. Phase 3's corpus has no adds or appends. Keeping `notes` free of
keys favours the fast path; shopping's tables all reference `lists`.

Mix, all arrivals, fast path against forced rewind (ms):

| Crossed | Run | Fast p50 / p95 / p99 / max | Rewind p50 / p95 / p99 / max | p95 ratio |
| ---: | ---: | --- | --- | ---: |
| 4 | 1 | 2.66 / 3.15 / 3.51 / 6.54 | 2.63 / 3.17 / 3.53 / 7.07 | 0.992 |
| 4 | 2 | 2.69 / 3.42 / 4.79 / 8.81 | 2.66 / 3.00 / 3.15 / 3.54 | 1.142 |
| 4 | 3 | 2.78 / 3.54 / 4.09 / 6.07 | 2.80 / 3.61 / 5.14 / 9.41 | 0.982 |
| 16 | 1 | 3.95 / 4.81 / 5.45 / 7.20 | 4.07 / 5.34 / 6.46 / 13.66 | 0.901 |
| 16 | 2 | 4.10 / 4.79 / 5.25 / 6.07 | 4.15 / 4.82 / 5.29 / 6.29 | 0.993 |
| 16 | 3 | 3.94 / 4.70 / 5.13 / 5.68 | 3.99 / 4.72 / 5.76 / 12.56 | 0.994 |
| 64 | 1 | 10.81 / 11.93 / 12.34 / 13.68 | 10.77 / 11.70 / 12.06 / 18.45 | 1.020 |
| 64 | 2 | 11.42 / 12.45 / 13.54 / 15.84 | 11.51 / 12.56 / 13.89 / 16.47 | 0.992 |
| 64 | 3 | 11.14 / 12.30 / 13.15 / 16.84 | 11.04 / 12.14 / 13.46 / 15.26 | 1.013 |

The target is a ratio of at most 0.80. No run reached it.

| Crossed | Direct arrivals | p95 ratio of those alone, runs 1 to 3 |
| ---: | ---: | --- |
| 4 | 200 of 640 | 0.828, 0.992, 0.803 |
| 16 | 200 of 640 | 0.686, 0.735, 0.712 |
| 64 | 0 of 640 | none |

## Why it failed

Where it applies, the path works: a direct arrival at 16 crossed is about 28%
cheaper. It applies too rarely to move a p95. The p95 needs at least 95% of
arrivals direct, and 62% of the mix is constrained, which the settled rule sends
to a rewind. At 64 crossed every arrival met a predicate, which is opaque, or a
constrained transaction. Phase 3 predicted this: it found no adds and few keyed
updates, and this result needs a different mix or wider constraint coverage,
such as unique and foreign-key proofs, to change.

Not measured: phones, and the phase-start commit as a third arm; the arms are
paired on one build. Forced rewinds in that build carry the `applied` index.

## Completion gate

The merge rule was applied: condition 1 held, condition 2 failed, the crate is
deleted. Final checks on the change without the crate:

| Check | Result |
| --- | --- |
| `./scripts/lint` | passed |
| `./scripts/check` | passed: 246 unit, 1,515 integration |

The tree's code equals phase 5's `c8db1fd9`, so the Kotlin and shopping checks,
which this phase's change does not touch, were not rerun.

Not verified: the e2e tier other than the fixtures in slice 4, and phones.
