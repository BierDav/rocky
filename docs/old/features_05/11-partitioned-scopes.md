# 11. Partitioned scopes

Status: COMPLETE. All completion gates qualified on 2026-10-08.
Owner: `rocky-credentials` selectors and envelopes,
`rocky-secure` scopes, frames and statement checks, `rocky-sdk` subscriptions,
the Kotlin builder. Depends on [phase 1](01-credential-primitives.md) and
[phase 7](07-row-identity.md); [phase 2](02-schema-builder-and-room-import.md)
for the builder. Wave 2. On the critical path; the credential slice may start
in wave 1 ([overlap](README.md#critical-path)).
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 4](agreed-scope.md#4-partitioned-scopes) of the scope, the
selector and envelope rows of [section 7](agreed-scope.md#7-credential-primitives),
[credentials limits](../runtime/credentials.md#limits),
[replica roles](../runtime/replica-roles.md),
[secure sessions](../runtime/secure-sessions.md) and the statement syntax
tree the engine builds in [secure SQL](../runtime/secure-sql.md).

A reader scope is one table today. A partitioned table declares partition
columns; each distinct tuple is its own scope with its own key, and a device
holding part of a table reads it only by naming held partitions or by asking
for a partial result with `row_kind = 0`. Per-list sharing in the shopping
app is the first test; the ERP is the target.

## Ordered slices

1. Decision record: partition tuples in scopes, grant selectors, key
   envelopes and key epochs per partition, the frame change, versioning and
   refusal.
2. Credentials: selectors over partitions ("table items, customer X, any
   warehouse"), narrowed by delegation; envelopes that wrap a partition key to
   a recipient key, to several recipients; key epochs per partition.
3. Schema: `partitionBy` in the builder; partition columns are part of the
   primary key and immutable; the identity index of phase 7 is validated.
4. Frames: the partition tuple is authenticated; a row cannot be sealed under
   another partition's key; every reader checks the author's insert, update
   or delete right in that partition.
5. Subscribe and evict per partition in the SDK; a device knows whether it
   holds a table fully.
6. Statement check on the syntax tree: a read, update or delete of a partly
   held table names held partitions by equality or `IN`, directly or through a
   join, or carries `row_kind = 0` on the whole table reference; views,
   views on views and `LEFT JOIN` `ON` clauses are checked; `OR` does not
   count; anything else is rejected. `row_kind = 1` or `2` is never accepted.
7. ERP-shaped measurement: 200 partitions on one device; record the per-scope
   overhead and set the budget in this brief.

## Completion gate

- A device with a grant on list A never receives or decrypts rows of list B;
  a frame sealed under the wrong partition key is refused.
- The same schema gives the same answer or an error on a partial, full and
  trusted node for the three statements in
  [the scope](agreed-scope.md#queries-on-a-partially-held-table).
- A `partial_<table>` view declared by the application passes the check.
- A custody node verifies the grant on a partition without its key.
- 200 partitions on one device: open, sync and checkpoint costs recorded and
  a budget written here.
- [Credentials](../runtime/credentials.md), [secure SQL](../runtime/secure-sql.md)
  and [replica roles](../runtime/replica-roles.md) describe partitions.


## Implemented boundary (2026-10-08)

[Decision 0034](../decisions/0034-partitioned-scopes.md) is the sole partition
record; phase 9 retains 0032. The dedicated jj workspace was rebased onto
`main = lsyzkyrv`, including phase 9's strong-history implementation.

- Canonical partition tuples, selectors, checked attenuation, paired grants,
  author admission and signed recipient-bound key envelopes.
- Definition/descriptor format 3, Kotlin `partitionBy`, compound identity
  indexes and Room overlays/application views. Format-2 bytes remain compatible.
- Authenticated frame format 3, wrong-image/scope/key refusal, keyless custody,
  per-partition author IDs and the own-id per-table exception. Checkpoints use
  format 4, preserving phase 9's unpartitioned format 3.
- Durable addresses and external SDK key storage, epochs, conservative
  revocation, pseudonym renewal, grant/delegate/install/subscription/eviction
  JSON and Kotlin APIs. Keys and grants are delivered explicitly by applications.
- Execution-time SQL proofs over bound equality/IN, constrained joins, nested
  application views and LEFT JOIN ON clauses. OR supplies no proof; reserved
  row kinds are refused. Cached unheld rows are filtered without destroying
  outer joins. Unsupported proof shapes fail conservatively.
- Trusted per-partition IDs and view reads; checkpoint/bootstrap/rebuild and
  eviction of one partition preserve the others' rows and permanent marks.

[The runtime guide](../runtime/partitioned-scopes.md) describes consumer setup,
key/ID lifecycle, completeness and conservative SQL refusals.

## Qualification

Runtime `partition_frames_keys_custody_sql_and_checkpoints_are_isolated` covers
A/B isolation, authorized range transfer, B-range refusal for an A reader,
keyless custody, wrong-partition authoring and signed legacy downgrade refusal,
bound execution values,
partial/full statements, nested partial views, checkpoint bootstrap, rebuild,
subscriptions and custody-backed eviction while B remains readable.
The trusted SDK test qualifies the agreed held-partition, complete aggregate
and partial-view statements and independent IDs on two converged replicas.
SDK public-request tests cover key envelopes, grants, supplied renewal,
pseudonyms, per-partition and own-id allocation, rotation/revocation and reopen.

Additional surfaces: storage SQL proof tests, SDK driver tests, Kotlin contract
checks, hand/Room descriptor qualification, actual schema/Room/SQLite driver
modules, native ABI checks and artifact-only packaged Kotlin consumption.
Final `./scripts/check` exited 0, including formatting, workspace Clippy/tests
and SDK tests without default features. The secure suite passed 126 tests;
its six explicit ignores include the separately run 200-partition measurement.
`./sdks/kotlin/check`, `./sdks/kotlin/check-schema`, `./sdks/native/check`,
`./sdks/kotlin/check-packaged` and the Kotlin schema/SQLite/Room modules
(18 tests) exited 0. All seven SDK driver tests passed. Final full-check evidence:
`local/phase11/check-final-qualified.log`.
Android device/emulator execution is a later packaging qualification, not
inferred from JVM/native checks. No distributed throughput claim follows from
in-process authenticated range transfer.

### 200-partition measurement and budget

Reproduce with:

```sh
cargo test -p rocky-secure --lib measure_200_partitions --locked -- --ignored --nocapture
```

One device holds 200 independently keyed partitions, one identity-bearing row
and one author log per partition. A second replica catches up through the
credential-checked frontier/range boundary; every partition is checkpointed.
Reopen includes reinstalling all external keys and rebuilding readable state.
Measurements on the development macOS host, debug build, shared machine:

| Cost | Measurement | Debug fixture budget |
| --- | ---: | ---: |
| Empty open | 118 ms | 250 ms |
| Register keys and author 200 rows | 9.011 s | 15 s |
| Authenticated range catch-up | 11.407 s | 25 s |
| 200 checkpoints | 4.324 s | 12 s |
| Reopen plus 200 key installations | 1.079 s | 5 s |
| Empty per-partition metadata | 389 B average | 2 KiB/scope |
| Populated fixture growth | 30.6 KiB/scope | 64 KiB/scope |

The empty schema is 107 SQLite pages; registering addresses/key hashes adds
19 pages for 200 scopes (4,096-byte pages). The populated/checkpointed file is
1,638 pages / 6,709,248 bytes, including 2,199,138 encoded ciphertext/frame bytes.
The populated budget includes this fixture's rows, credentials, logs and
checkpoints; it is not a bound for arbitrary partition size. CPU/wall-time
thresholds are regression review budgets on comparable hardware, not timing
assertions in deterministic correctness tests. Native key-provider and real
network latency are additional costs. Evidence logs are under ignored
`local/phase11/`.

## Reconciliation with phases 12 and 13

Landed after [phase 12](12-hidden-rows-unique-indexes-and-foreign-keys.md)
and [phase 13](13-confirmed-rows-in-room.md). One descriptor format applies:
format 2 keeps unpartitioned descriptors, with or without constraint policies
and delete actions; format 3 adds `partition_by` and views. Identity columns,
which foreign keys reference, are the partition columns followed by `id`.
Constraint resolution and confirmed rows still run within one unpartitioned
scope, so installation refuses conflict policies and foreign keys on
partitioned tables, foreign keys to them, and partitioned command-controlled
tables. Supporting them is follow-up work.
