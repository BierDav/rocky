# 3. Catch-up cost

Status: complete (2026-10-10). [Measured evidence and qualification](../../benchmarks/runtime-scaling/evidence/catch-up-2026-10-10/README.md).
Owner: `rocky-secure` apply (`replica/incoming.rs`, `replica/group.rs`), the
SQL writer lease (`replica/sql.rs`), Room driver profile (`rocky-sdk/connection.rs`),
credential verification on receive and the group-sync measurement fixture;
the shopping device procedure. No dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the entry on a Room write timing out in
[fix later](../fix_later/README.md), the
[emulator pair](../runtime/shopping-devices.md#emulator-pair-2026-10-09)
and its [procedure](../runtime/shopping-devices.md#two-phone-procedure),
[the writer](../runtime/sql-driver.md#the-writer), the
[partition budget](../runtime/partitioned-scopes.md#limits-and-budget) and
[decision 0044](../decisions/0044-group-frontier-exchange.md).

Two costs of catching up are known and neither is explained. A Room write
failed with code 4 (`transaction was rolled back: Timeout`) in two of nine
emulator runs, right after a device came online. And the four-table ERP
fixture, `measure_200_partition_group_sync` in
`crates/rocky-secure/src/tests/group_sync_measurement.rs`, misses three
budgets of four times the single-table ones: register and author 86 s of
60 s, catch-up 339 s of 100 s, checkpoints 121 s of 48 s. That is 11.6 times
the single-table cost for four times the tables. Measure both, then fix what
needs no format change.

## Settled

- Apply already commits at most 64 operations per transaction
  (`process_pending`, `GROUP_REQUESTS`). What it does not do is release the
  replica between those transactions.
- The cause of the Room timeout is not established. The 5 s lease of a SQL
  transaction starts once `begin` holds the replica, so a writer waiting for
  the replica does not by itself produce that error. Reproduce it before
  changing apply.
- "Write limit" means: from the BEGIN request to the return of COMMIT, under
  the 5 s default `transaction_timeout`.
- The working hypothesis for the fixture is credential resolution repeated
  per frame or per scope, not chain bytes. The profile decides.
- The profile must attribute at least 80% of each missed budget to named
  costs.
- Whatever the profile attributes to chain bytes in frames and pages is left
  to [phase 11](11-chains-by-reference.md), which starts from this profile.

## Ordered slices

1. Reproduce the timeout deterministically in `rocky-secure` or `rocky-sdk`:
   a SQL writer against a replica that receives a 10,000-operation catch-up.
   Record which wait expires and the longest replica hold. If it cannot be
   reproduced there, reproduce it on the emulator pair with timing recorded
   around the lease and the replica lock.
2. Fix the cause found. If it is the hold: apply releases the replica after
   each committed transaction and a waiting writer goes first. If it is
   elsewhere, fix it there and keep apply unchanged.
3. Profile the fixture per missed budget: time in chain verification and
   credential resolution, signatures, sealing, storage and flushes, and round
   trips; and bytes of chains against payload. Record it beside the budgets
   in the partition guide.
4. Fix the costs the profile names that need no persisted or wire format
   change, largest first, such as verifying one chain once per session or
   pass instead of once per frame. Verification stays per request against the
   current policy; only repeated identical work is removed.
5. Rerun the fixture and the emulator procedure. Record the result, and hand
   phase 11 the remaining attribution in the partition guide.
6. Remove the Room-write entry from fix later and update the driver guide.

## Completion gate

- A regression test fails on the unfixed code with the recorded cause and
  passes after slice 2.
- A writer that requests BEGIN during a 10,000-operation catch-up returns
  from COMMIT within the write limit; the longest replica hold is recorded.
- A catch-up interrupted between any two apply transactions resumes and
  converges; no operation is applied twice.
- The profile attributes at least 80% of each of the three missed budgets.
- Each budget is met, or its remainder is attributed to chain bytes or to a
  named cost that needs a format change, with seconds.
- Twenty emulator-pair runs of the device procedure show no write timeout
  and no retried write.
- `cargo test -p rocky-secure` and `./scripts/check` pass; the fixture stays
  an ignored measurement.

## Not in this phase

- Chains by reference, the checkpoint page's chains and CBOR byte strings
  ([phase 11](11-chains-by-reference.md)).
- Any change to a frame, a stored operation, a checkpoint page or the
  session; a fix that needs one is recorded for phase 11, not made.
- The sync worker, back-off and the pause
  ([phase 2](02-change-driven-sync.md)).
- A longer write limit or more driver retries as the fix.
- Reads on the writer thread
  ([known costs](../runtime/scaling-limits.md#known-costs)).
- Caching a verification result across policy changes or sessions.
