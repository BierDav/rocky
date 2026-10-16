# Phase 15 evidence

## Final seals

The [accepted proposal](15-lazy-compaction.md#proposal-final-seals) is
implemented as [final seals](../runtime/seal-finality.md): terminal
acknowledgments, finality records and the genesis line's minimum, with
coordinator store 9, link `rocky.link.v8` and replica file
`rocky.encrypted-replica.v24`.

One deviation from the proposal: any trusted node may acknowledge the
committed terminal, not only the in-sync set at the terminal. A log no node
joined still has one terminal, and safety rests on the minimum of distinct
trusted signers.

`a_seal_its_log_committed_becomes_final_and_beats_a_later_owner_seal`
(`rocky-secure`) ends a tip's log, finalizes the seal through trusted
acknowledgments, refuses a later owner seal and compacts the final seal at
`keep = 0` at once. In `rocky-credentials`,
`a_final_seal_beats_the_owners_and_revoking_its_acknowledgers_resolves_again`
checks the resolution order and revocation below the minimum.

## Slice 1: finalization and compaction at seals

See the [guide](../runtime/lazy-compaction.md). Checkpoint format 14 names
its seal. Tests in `rocky-secure` `tests::compaction`:

| Gate item | Test and result |
| --- | --- |
| Plateau: 64 insert/delete cycles leave no tombstones or stubs | `compaction_at_seals_plateaus_under_insert_delete_churn`: compacting at seals after cycles 16 and 64 leaves one live row, its stub, no operations, no applied entries, one finalized seal and no bounds or drops; the counts are equal at both. Existence records are slice 2's |
| A compacted and an uncompacted device apply the same late frames across two `continue` seals alike | `compacted_and_uncompacted_devices_apply_late_frames_across_two_seals_alike`: A compacts, B only finalizes; C's offline frames walk both seals; A, B and C show the same rows |
| Without a log, one seal left uncompacted survives a winning concurrent seal; none left needs a rebootstrap | `a_seal_left_uncompacted_that_loses_is_rebuilt_and_a_compacted_one_needs_a_rebootstrap`: with `keep = 1` the device rebuilds from its base at the first seal and ends on the winner; with `keep = 0` the switch is refused and `rebootstrap` names the lost seal. The rebootstrap itself is slice 3's |
| A final seal is compacted at once | The finality test above |
| Compaction survives reopening | `compacting_at_a_seal_leaves_only_live_rows_and_survives_reopen` |

The SDK `tips` actions `compact` and `compaction` and Kotlin
`Tips.compact`/`Tips.compaction` are covered by
`compact_keeps_the_requested_seals_and_every_row_across_reopen`
(`rocky-sdk` tips) and `compactSendsKeepAndDecodesTheReport` (Kotlin
`TipsTest`).

Fixed on the way: a closed coordination domain still answered head reads,
so a member already in the in-sync set began a coordinated transaction.
Coordinated heads now carry `closed`, and the slot is refused
(`a_registered_service_domain_commits_ordinary_rows_and_closes`, which
failed before this phase).

### Verification, 2026-10-16

- `./scripts/lint`: passed.
- `./scripts/check`: passed; 167 unit tests and 996 integration tests within
  their budgets.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed.
- `./examples/shoppinglist-app/check`: 57 of 58 JVM tests passed.
  `SchemaUpgradeTest` fails as recorded in
  [phase 14](14-seals-and-bookmarks-evidence.md#what-remains-before-phase-14-is-implemented)
  (adoption on discovery waits for phase 17). It fails the same way without
  slice 1, on the final-seal change. The Android tests and the desktop and Android
  builds did not run, because the script stops at that failure.

### Open in slice 1

- Domains with existence-log tables do not finalize; slice 2.
- Scopes pruned under arbitrary-cut checkpoints before the graph cannot
  finalize until a checkpoint at a seal replaces them; slice 3.
- Partial holders never finalize locally; slice 2's and 3's issuer bases.
- Change detection is per table, not per partition.
- `seal_waits` scans every stored header on each maintenance step while a
  seal waits.
- In a graph domain the SDK's checkpoint-first join finds a checkpoint only
  after a compaction.
