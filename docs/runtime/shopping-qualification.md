# Shopping qualification

This guide identifies the shopping app's current checks and the limits of
older qualification evidence. Phase 15 removes the declaration-based release
gate; [phase 20](../features_09/20-seal-qualification.md) qualifies the app on
seals. The native seal-compaction evidence is in
[phase 15](../features_09/15-lazy-compaction-evidence.md).

## Commands

| Command | Runs |
| --- | --- |
| `./examples/shoppinglist-app/check` | Required schema, JVM and Android shared tests, desktop and Android builds; stops at the first failing step |
| `./examples/shoppinglist-app/qualify` | Packaged peer and [multi-process harness](../../examples/shoppinglist-app/qualification/src/Qualification.kt), with Room repositories in separate JVMs; evidence in `local/shopping-qualification/evidence.json` |
| `cargo test -p rocky-sdk --test shopping_release` | Native custody, move/checkpoint join and restart scenarios |
| `cd examples/shoppinglist-app && ./kotlin run -m qualification -- required-schema` | Required schema by hand and from Room, installed natively |
| `sdks/kotlin/gradlew -p sdks/kotlin :qualification:schema:check` | Hand/Room equivalence and partitioned schema refusals |
| `./examples/shoppinglist-app/qualify-live-links <command>` | Independent JVM processes over isolated iroh; see [commands](../../examples/shoppinglist-app/README.md#verify) |

`ROCKY_SCENARIOS=gate,relink` selects harness scenarios. App modules contain
no Rust source. The custody process is the packaged
[always-on peer](always-on-peer.md).

## Current scenarios

The `gate` scenario alternates phones A and B through a keyless custody peer:
they share lists, add duplicate titles offline, move an item while its other
holder ticks it offline, and converge. Deleting a category hides its links;
deleting a list cascades to its items. An offline phone returns and catches
up to those deletes. The removed declaration and finalization requests are
no longer part of this harness, and it makes no seal-compaction qualification
claim. Native seal probes are listed in [lazy compaction](lazy-compaction.md#evidence).

| Scenario | Coverage |
| --- | --- |
| Alternating phones through custody | Harness `gate`; native `two_phones_never_online_together_pass_the_release_gate_through_custody` |
| Per-list exclusion and revocation | Harness `exclusion-revocation`; SDK `group_secrets` |
| Offline relink and duplicate titles | Harness `relink` and `gate` |
| Creator loss and second-issuer renewal | Harness `creator-loss`; `IssuerRecoveryTest` |
| Joining after a move | Native `a_device_joining_after_a_move_starts_the_list_from_a_checkpoint` |
| Custody restart | Native `custody_keeps_offered_partitions_across_restart` |
| ERP partition sync | `measure_200_partition_group_sync`, `erp_group_sync_preserves_all_references_and_reopens` |

Approval numbering uses [coordinated writes](coordinated-writes.md). The
isolated-iroh harness does not configure a coordinator, so it does not qualify
that path across processes. `ApprovalsTest` exercises the application's
configured JVM coordinator path separately.

## Constraints and moves

The native regressions cover partition-local key winners, authenticated
frames/checkpoints, group cascade and SET NULL, eviction and reopening.
`partitioned_latest_wins_isolated_identity_rebuild_eviction_and_reopen`,
`partition_frames_keys_custody_sql_and_checkpoints_are_isolated` and
`group_references_hold_cascade_and_null_only_category_per_partition` remain.
Schema qualification refuses cross-partition uniqueness, natural-key foreign
references, RESTRICT, confirmed partition rows and references between groups.

Move checks include `foreign_author_move_follows_offline_edit_once_and_keeps_identity`,
`source_only_author_drops_offline_update_and_cannot_open_destination`,
`destination_edit_with_later_stamp_wins_over_offline_update`,
`attachments_move_in_same_bundle_category_nulls_and_b_only_reader_rebuilds`
and `missing_child_holding_and_interrupted_move_leave_everything_unchanged`.
`six_scenarios_across_device_processes` runs their process fixture.

## Historical evidence and remaining work

Earlier JVM and emulator runs are historical evidence for prior protocols
and schemas. They do not qualify the current seal-based release gate. The
old JVM gate ran over loopback iroh, not Internet or NAT conditions. Current
[device procedures and emulator evidence](shopping-devices.md) remain available.

Current full-script verification is recorded in the
[phase 15 evidence](../features_09/15-lazy-compaction-evidence.md#verification-of-slices-4-and-5).
`SchemaUpgradeTest` awaits phase 17's restoration of adoption on discovery.
When that JVM failure stops `check`, its Android tests and builds have not run.

## Outstanding

Two physical-phone runs remain outstanding. The procedure passed on two
emulators; see [shopping devices](shopping-devices.md#emulator-pair-2026-10-09)
and the [two-phone procedure](shopping-devices.md#two-phone-procedure).
Cellular, relay, lifecycle and radio costs need their own measurements.
