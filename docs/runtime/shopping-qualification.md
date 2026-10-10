# Shopping qualification

Qualified on 2026-10-09 by [phase 10](../features_06/10-qualification.md) on
the JVM desktop profile (macOS arm64, loopback TCP), with each device in its own
process, and its sharing and move steps on two Android emulators. Phone runs, the
market scenario on devices and horizons through custody alone are [outstanding](#outstanding). This is not an Internet, NAT or relay claim.

## Commands

| Command | Runs |
| --- | --- |
| `./examples/shoppinglist-app/qualify` | Builds the packaged `rocky-peer`, then the [multi-process harness](../../examples/shoppinglist-app/qualification/src/Qualification.kt): every device is a JVM running the app's Room repository; evidence in `local/shopping-qualification/evidence.json` |
| `cargo test -p rocky-sdk --test shopping_release` | The release gate, a checkpoint join after a move and custody restart, in `./scripts/check` |
| `./kotlin run -m requiredSchema` | The required schema by hand and from Room, installed natively |
| `./kotlin check qualify -m schema-qualification` | Hand/Room equivalence and every partitioned refusal |
| `cargo test -p rocky-secure --lib <name>` | The native tests named below, in `./scripts/check` |

`ROCKY_SCENARIOS=gate,relink` selects harness scenarios. The application
modules contain no Rust source; the custody process is the packaged launcher
([always-on peer](always-on-peer.md)).

## Release gate

The harness's `gate` scenario: phones A and B are never online together and
meet only the custody process, which restarts in between. A and B share three
lists through the Share button's codes, add the same title offline (one survives
by `LatestWins`), and A moves an item B ticked offline: the item arrives in the
other list ticked, its category nulled. Deleting a category nulls only
`items.category`; deleting a list cascades to its items. A horizon on all nine
group-partition table scopes finalizes on A and B, and phone C, which joined from
checkpoints and stayed offline across the horizon, rebootstraps to the finalized
state. `two_phones_never_online_together_pass_the_release_gate_through_custody`
repeats it natively. Measured: see [evidence](#evidence).

For the horizon, B meets A directly; see [outstanding](#outstanding).

## Acceptance scenarios

The deliverable is the shopping-list application as a packaged Kotlin app with
no Rust source: Room DAOs on Rocky's file, lists, categories and items in one
partition group, per-list sharing through one envelope, a packaged always-on
custody peer and a second issuing device. Its lists merge; its approvals are
strong commands the public coordination service orders, which this harness
does not run: its devices open over TCP without a coordinator. Strong commands
under contention, loss, takeover and voter faults are qualified by the
[market](market-qualification.md).

| Scenario | Evidence | Status |
| --- | --- | --- |
| 1 Phones alternate through custody; checkpoint join | Harness `gate`; native `shopping_release` | Passed on JVM processes |
| 2 Per-list exclusion and revocation | Harness `exclusion-revocation`; `group_secrets` tests | Passed on JVM processes |
| 3 Offline relink and duplicate title policy | Harness `relink` and `gate` | Passed on JVM processes |
| 4 Creator loss; second issuer invites and renews | Harness `creator-loss`; `IssuerRecoveryTest` | Passed on JVM processes |
| 5 All-device horizon and long-offline rebootstrap | Harness `gate` | Passed on JVM processes |
| 6 Market 4,096 commands, DAO stock, keyed read | `./examples/market-app/qualify` (five scenarios), `./examples/market-app/soak`; [market qualification](market-qualification.md) | Passed on JVM processes; devices [outstanding](#outstanding) |
| 7 ERP fixture, 200 partitions | `measure_200_partition_group_sync`, `erp_group_sync_preserves_all_references_and_reopens` | Measured; see [evidence](#evidence) |

## Partitioned constraints acceptance

| # | Evidence |
| --- | --- |
| 1 | `./kotlin run -m requiredSchema` passes |
| 2 | `partitioned_latest_wins_isolated_identity_rebuild_eviction_and_reopen`; harness `gate` |
| 3 | `partition_frames_keys_custody_sql_and_checkpoints_are_isolated`; harness `exclusion-revocation` |
| 4 | `group_references_hold_cascade_and_null_only_category_per_partition`, `group_horizon_finalizes_losers_orphans_and_recovers_each_atomic_step`; harness `gate` |
| 5 | `partition_checkpoint_rebuilds_keyless_support_and_base_import_preserves_unrelated_graph`, the eviction cases above; `a_device_joining_after_a_move_starts_the_list_from_a_checkpoint` |
| 6 | The ERP measurement below, against [the budget](partitioned-scopes.md#limits-and-budget) |
| 7 | [Refusal regressions](#refusal-regressions) |

## Move acceptance

| # | Evidence |
| --- | --- |
| 1 | `foreign_author_move_follows_offline_edit_once_and_keeps_identity`; harness `gate` |
| 2, 3 | `source_only_author_drops_offline_update_and_cannot_open_destination` |
| 4 | `destination_edit_with_later_stamp_wins_over_offline_update` |
| 5 | `attachments_move_in_same_bundle_category_nulls_and_b_only_reader_rebuilds` |
| 6 | `missing_child_holding_and_interrupted_move_leave_everything_unchanged` |

`six_scenarios_across_device_processes` runs all six with each device in its own
process. Phone runs are outstanding.

## Refusal regressions

Each shape under [not requested](../features_06/scope-partitioned-constraints.md#not-requested)
and every reference between groups is refused natively and in Kotlin:

| Shape | Native | Kotlin (`schema-qualification`) |
| --- | --- | --- |
| Uniqueness across partitions | `partitioned_tables_refuse_incomplete_indexes_references_and_command_control` | `must include every partition column` |
| `RESTRICT` | `partitioned_foreign_keys_refuse_restrict` | Room `onDelete RESTRICT` |
| Natural-key reference | `partitioned_foreign_keys_refuse_natural_keys` | builder and Room identity messages |
| Partitioned command control | as uniqueness | `cannot be command-controlled` |
| Confirmed rows of a partitioned table | `partitioned_confirmed_rows_are_refused_on_every_profile` | `cannot hold confirmed rows` |
| Reference between groups | `foreign_keys_refuse_different_partition_groups`, `foreign_keys_refuse_unlogged_parent_outside_group` | `needs the same explicit partition group` |

## Defects fixed during qualification

- A keyless custody peer could not hold a list created after it enrolled, and a
  device syncing with it failed its whole batch as an end of file. Devices now
  offer partition tuples to custody ([decision 0045](../decisions/0045-custody-partition-offers.md)).
- Two issuers that each created a list before meeting ended every session with
  each other, so neither list could be shared. Each side now refuses only the
  list it has not learned ([decision 0046](../decisions/0046-per-scope-frontier-refusal.md)).
- Accepting a second list while a checkpoint-first bootstrap waited for a
  checkpoint peer was refused; it now joins the pending intent.
- Importing a checkpoint of a list an item had moved out of failed on the
  imageless tombstone, so no device could start that list from a checkpoint.
- A participant finalized a parent table before its child, after which the
  child's horizon could never be acknowledged. Finalization now waits for
  pending dependent children ([horizon](mergeable-horizon.md#dependencies-and-ordering)).
- The Share button shared with the custody peer, which holds no key, and
  failed; it now skips custody devices.
- The trusted profile installed a partitioned confirmed table.
- The Compose screen crashed when a DAO flow's read transaction met an
  authority change or an incomplete list; the repository observes again.

## Evidence

Recorded on 2026-10-09; see the [phase brief](../features_06/10-qualification.md#evidence).

## Outstanding

| Scenario | Blocking contract |
| --- | --- |
| Two-phone runs of the gate, moves and revocation | Two physical Android phones on the qualification host, running the [device procedure](shopping-devices.md#two-phone-procedure). Only one phone, in personal use, was attached; the procedure [passed on two emulators](shopping-devices.md#emulator-pair-2026-10-09) |
| 6 Market on devices | Two phones competing for the last item, in [phase 22](../features_07/22-qualification.md). JVM processes and one emulator reservation passed; see [market qualification](market-qualification.md) |
| Horizon through custody alone | A keyless node that admits signed horizon statements and forwards participants' signed acknowledgments; no such contract exists, so participants meet an issuer |
| Internet, NAT and relays | A run over iroh relays between networks; only loopback and one LAN were used |

The earlier single-phone LAN run is recorded in [shopping devices](shopping-devices.md).
