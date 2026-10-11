# 10. Qualification

Status: JVM gate passed on 2026-10-09; the two-phone runs, scenario 6 (market) and
horizons through custody alone remain outstanding with their blocking contracts in
[the audit](../runtime/shopping-qualification.md#outstanding).
Owner: the shopping example, Android qualification and the audit. Depends on
every earlier phase. Wave 5.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the audit](../runtime/shopping-qualification.md), its
[acceptance scenarios](../runtime/shopping-qualification.md#acceptance-scenarios)
and the acceptance lists of both scopes. The earlier qualification stopped because the shopping schema did not install.
This phase runs its outstanding scenarios and the new ones, and records
evidence.

## Ordered slices

1. Phase 16's scenarios 1 to 5 and 7 on the group-based schema; scenario 3 as
   resolved in [the scope](scope-partitioned-constraints.md#resolved-questions).
   Scenario 6 (market) is outside this roadmap; the audit keeps it listed as
   outstanding with its own blocking contract.
2. Acceptance 1 to 7 of [partitioned constraints](scope-partitioned-constraints.md#acceptance)
   in CI, multi-process where noted.
3. [Move acceptance](scope-moving-rows.md#acceptance) in CI and on two phones.
4. Sharing and revocation through the group secret on two phones with the
   always-on custody peer.
5. Refusal regressions for every shape listed under
   [not requested](scope-partitioned-constraints.md#not-requested) and for
   references between groups.
6. Update the audit, the getting-started guide .

## Completion gate

- Every scenario above has a reproducible command or device procedure and
  recorded evidence.
- The [release gate](README.md#release-gate) is shown with no Rust source in
  the application.
- The audit lists no outstanding scenario, or names each remaining one with
  its blocking contract.
- `./scripts/check` and the Kotlin, native and packaged checks pass.

## Evidence

Recorded on 2026-10-09 on a MacBook Pro (Apple M5 Pro, 18 cores, 48 GB,
macOS 27.0.1, Rust 1.99.0, debug builds). The audit maps each scenario and
acceptance item to its command; this records results.

### Slices

1. `./examples/shoppinglist-app/qualify`: `gate`, `exclusion-revocation`,
   `relink` and `creator-loss` pass, each device a separate JVM on loopback TCP
   and custody the packaged `rocky-peer`: 63.8 s, 11.9 s, 8.7 s and 9.1 s.
2. Constraint acceptance: `./kotlin run -m requiredSchema` passed (47 s). The
   native tests named in the audit pass in `./scripts/check`. ERP, 200
   partitions (`measure_200_partition_group_sync`), against the phase 9 budgets:

   | Cost | One table | Four tables | Four-table budget |
   | --- | ---: | ---: | ---: |
   | Register and author | 7.4 s | 86.0 s | 60 s, missed |
   | Catch-up | 24.5 s | 338.9 s | 100 s, missed |
   | Idle sync round | 0.25 s | 1.43 s | 10 s |
   | Checkpoints | 5.4 s | 120.6 s | 48 s, missed |
   | Reopen | 0.9 s | 7.9 s | 20 s |
   | Metadata per partition | 410 B | 1,577 B | 8 KiB |

   The three misses are phase 9's, unchanged; idle sync stays within budget
   with custody offers (1.37 s in phase 9). The single-table probe
   `measure_200_partitions`, repaired for per-author identity allocation,
   passes every [budget](../runtime/partitioned-scopes.md#limits-and-budget):
   initialization 7.1 s, catch-up 11.8 s, checkpoints 5.4 s, reopen 1.7 s.
3. Moves: the six native cases and `six_scenarios_across_device_processes`
   pass in `./scripts/check`; the harness `gate` moves an item ticked offline.
   On two Android 15 emulators the device procedure moved an item ticked
   offline through custody in five of five runs
   ([emulator pair](../runtime/shopping-devices.md#emulator-pair-2026-10-09)).
   Two phones: outstanding.
4. Sharing and revocation through the group secret with custody: harness
   `gate` and `exclusion-revocation`; the emulator pair shares two lists
   through custody alone. Revocation on devices and two phones: outstanding.
5. Refusals: native and Kotlin checks for every shape, listed in the audit;
   `partitioned_confirmed_rows_are_refused_on_every_profile` found and fixed a
   trusted-profile gap.
6. The audit, [shopping devices](../runtime/shopping-devices.md) and the
   [getting-started guide](../runtime/getting-started.md) are updated.

Final gates: `./scripts/check` (813 passed, 17 ignored), `./kotlin check`
(53 tests, schema and inventory qualification, packaged consumer),
`./sdks/native/check`, `./examples/shoppinglist-app/check` and
`./kotlin test -m room-qualification` (13 tests) passed. The `shopping` CI job
has not run on a hosted runner.

Qualification fixed seven defects, listed in the audit;
[decision 0045](../decisions/0045-custody-partition-offers.md) records the
native session v6.
