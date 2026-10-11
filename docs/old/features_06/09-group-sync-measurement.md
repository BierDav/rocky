# 9. Group sync measurement

Status: complete; the completion gate passed on 2026-10-09. Three review-budget
misses remain recorded below.
Owner: `rocky-secure` measurement fixture; `rocky-sdk` sync. Depends on phases
[3](03-group-secrets.md), [5](05-partitioned-foreign-keys.md) and
[6](06-horizon-checkpoints-bootstrap.md). Wave 4.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the 200-partition budget](../runtime/partitioned-scopes.md#limits-and-budget)
and acceptance 6 of [the scope](scope-partitioned-constraints.md#acceptance).
The group secret makes key delivery independent of depth. At the assigned base,
the SDK synchronized scope by scope, with several round trips each, and kept
one log and checkpoint per table and partition. A four-table group over 2,000
customers is 8,000 scopes per sync. This phase measures that cost and removes
it only if it exceeds a budget.

## Ordered slices

1. ERP fixture: 200 partitions; a group of four tables (customer, order,
   order line, attachment) with in-group references; order lines also
   reference an unpartitioned logged `products`.
2. Measure open, authoring, catch-up, an idle sync round, checkpointing,
   reopen and metadata per partition, for one table and for four. Report the
   cost per additional table.
3. Set the budget in this brief, next to the existing one.
4. If the idle round or catch-up exceeds it: exchange frontiers for all
   scopes of a group partition in one round trip. Stored data does not
   change; a wire change gets a decision record.

## Completion gate

- The measurement is reproducible with one documented command and recorded
  with host and build profile.
- The brief states the cost per additional table of a group and the budget.
- An idle sync of 200 partitions with four tables stays within the budget,
  with or without slice 4.
- No correctness test asserts a timing.

## Review budgets

Set before the phase 9 run, for a debug build on the recorded host. The
single-table budgets in [partitioned scopes](../runtime/partitioned-scopes.md#limits-and-budget)
remain the historical baseline. Four tables retain four scopes per partition:

| Cost, 200 partitions | Existing one-table budget | Four-table group budget |
| --- | ---: | ---: |
| Open | — | 5 s |
| Register derived keys and author rows, combined | 15 s | 60 s |
| Authenticated bidirectional catch-up | 25 s | 100 s |
| Idle authenticated sync round | — | 10 s |
| Checkpoint all scopes | 12 s | 48 s |
| Reopen and reinstall derived keys | 5 s | 20 s |
| Empty metadata per group partition | 2 KiB | 8 KiB |

These scale the existing per-table review bounds by four; idle sync has an
independent 10-second bound for a foreground refresh. These are local loopback
review budgets, excluding WAN latency. Exceeding catch-up or idle requires slice 4.

## Reproduce

From the workspace root, run:

```sh
cargo test -p rocky-secure --lib measure_200_partition_group_sync --locked -- --ignored --nocapture
```

The fixture compares one and four ERP tables across 200 customer partitions.
Each partition authors one atomic bundle with one row in each member. Customers,
orders, order lines and attachments form an in-group `CASCADE` chain; order
lines also reference one unpartitioned logged product. Both cases include the
product scope (201 versus 801 total scopes). The one-table case keeps products
as a constant baseline without an order-line reference. Derived table keys
use a fixed synthetic test secret bound to each group address at epoch 1; the
probe measures derived scope keys, not secret-envelope delivery.

Each sync round uses mutually authenticated loopback TCP sessions with
TCP_NODELAY, as the SDK TCP transport does. They run the SDK row-sync sequence:
exchange batched frontiers, pull then push changed scopes, sync existence logs,
acknowledge mergeable horizons, and close. It resumes exhausted sessions, up to the SDK limit of 16, with
the existing 32 MiB session and 128 KiB range-page bounds. No admitted horizon
or command results are present. No network latency is injected. Registration and key installation are reported separately
from authoring; authoring includes the durability flush. Checkpointing includes
the product scope. Reopen includes schema admission, credentials and derived
key reinstallation, but excludes the subsequent row-count and audit checks.

Metadata is the logical SQLite page-count × page-size increase after registering
empty partitions and installing keys, divided by 200; it excludes the base
schema, row/log/checkpoint bytes, WAL and external secret-store files. Each
additional-table result is `(four - one) / 3`, across the full 200-partition run.
The ordinary two-partition regression exercises all four tables, checks visible
rows after catch-up and reopen, and audits every reopened scope. Neither test
asserts timing.

## Measurement on 2026-10-09

Host: MacBook Pro Mac17,8, Apple M5 Pro, 18 CPU cores, 48 GB RAM,
macOS 27.0.1 arm64. Rust 1.99.0 (`b940084d7`); Cargo test profile,
unoptimized with debuginfo. No workspace build/test gate ran concurrently.
[Recorded command output](09-group-sync-measurement-results.txt) contains the
one passing measurement test, session counts and exact additional-table values.

Timings below are seconds; metadata is bytes per partition. An additional table is the average marginal cost
across 200 partitions, `(four - one) / 3`; it includes the added references and
constraint graph, not only an empty table's key/log overhead.

| Cost | One table | Four tables | Per additional table | Four-table budget |
| --- | ---: | ---: | ---: | ---: |
| Open | 0.055 | 0.074 | 0.00635 | 5 |
| Register derived keys | 0.815 | 19.140 | 6.10817 | combined below |
| Author and flush | 5.694 | 61.944 | 18.74992 | combined below |
| Registration + authoring | 6.509 | 81.084 | 24.85809 | 60 |
| Catch-up | 23.004 | 373.439 | 116.81183 | 100 |
| Idle sync | 0.227 | 1.368 | 0.38027 | 10 |
| Checkpoints | 5.330 | 127.003 | 40.55772 | 48 |
| Reopen | 0.856 | 7.278 | 2.14091 | 20 |
| Empty metadata, B/partition | 409.60 | 1,617.92 | 402.77 | 8,192 |

The four-table catch-up used two bounded sessions; its idle round used one.
The initial unbatched one-table idle probe took 14.184 seconds, already above
the four-table bound, triggering slice 4. The implemented batch exchange meets
the idle completion gate. [Decision 0044](../decisions/0044-group-frontier-exchange.md)
records native session v5; stored formats stay unchanged.

Registration/authoring, catch-up and checkpointing still exceed their review
budgets. Batching removes empty scope exchanges; it does not remove per-table
key validation, operation admission, constraint work or checkpoint creation.
These remain measured limitations, not passing throughput claims. The brief's
completion gate requires idle sync within budget and batching when idle or
catch-up exceeds it; it does not require every review bound to pass. Reopen,
metadata and open passed, and all four tables retained 200 visible rows after
catch-up and reopen. The probe audited all 801 reopened scopes. Audits and
row-count checks are excluded from the timed reopen interval.

## Verification on the assigned base

On 2026-10-09, parent `9a5368dffb1b`:

- `cargo fmt --all -- --check`, workspace Clippy with `-D warnings`,
  `./scripts/check` and `./scripts/commitlint --jj` passed.
- Workspace: 658 tests passed, zero failed, 12 ignored, including doctests.
  SDK without default features: 129 passed, zero failed, 4 ignored.
- Native ABI: 2 tests passed; formatting, Clippy with bindgen and the build
  without default features passed.
- `./kotlin check`: 51 JUnit tests passed, zero failed or skipped.
- Separate Room gate: 12 tests passed. Separate schema gate: seven
  hand/import pairs installed and reopened; 5 schema JUnit tests passed.
- Separate packaged-consumer gate: one clean artifact-only consumer run passed.
- Kotlin initially refused a stale Android runtime. Publishing from this
  workspace built both configured ABIs; every Kotlin gate passed afterward.

The new regressions cover ERP row visibility and reopen/audit, unchanged
frontiers with a new horizon, all-or-nothing authorization of a batch, bounds
and preservation of complete selected groups, selector resolution equivalence,
and the SDK's upgrade signal when an old client is idle. No correctness test
asserts timing. Physical devices, WAN latency and maximum-author frontiers
remain unqualified.
