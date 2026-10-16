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

- Domains with existence-log tables do not finalize; slice 2 lifts this.
- Scopes pruned under arbitrary-cut checkpoints before the graph cannot
  finalize until a checkpoint at a seal replaces them; slice 3.
- Change detection is per table, not per partition.
- `seal_waits` scans every stored header on each maintenance step while a
  seal waits.
- In a graph domain the SDK's checkpoint-first join finds a checkpoint only
  after a compaction.

## Slice 2: existence logs at seals

See [existence logs](../runtime/lazy-compaction.md#existence-logs). Row frame
11 counts each commit's existence records; a holder of the scope key refuses
a frame claiming more than its creations and deletions. Compacting folds the
records a seal includes into a base at the seal and releases dead rows and
their records. The SDK signs that fold after `compact` when the device may
sign tip statements (`SignedExistenceBase`, format 1), and child-only or late
devices import it through the existing base transfer. The existence base
types moved out of the horizon credentials. Replica file
`rocky.encrypted-replica.v25` and link `rocky.link.v9` refuse earlier frames
and base encodings.

| Gate item | Test and result |
| --- | --- |
| Plateau without existence records | `a_holder_folds_the_existence_log_at_a_seal_and_a_child_holder_imports_its_signed_base` (`rocky-secure` `tests::existence::seals`): after two lists, four create/delete cycles and a deleted list, compacting leaves no records and one liveness row, the live list's. The dead ids stay retired |
| A child-only device imports the issuer's base and resolves the same orphans as a holder of the parent | The same test: C joins after the records were released, imports the owner-signed base in pages of two and shows the same items as A and B; the item of the deleted list stays an orphan. Later records continue every log past the base |
| A seal waits for the records its frames count | `a_holder_waits_for_every_record_its_frames_count`: a list holder without records reports `missing existence records`; after the records arrive it compacts and keeps none |

An unsigned fold is never offered: `existence_base_offer` returns none until
it is signed, a peer's fold is never imported, and a pusher skips a peer
whose base already covers its own.

### Verification, 2026-10-16

- `./scripts/lint`: passed.
- `./scripts/check`: passed; 167 unit tests and 998 integration tests within
  their budgets.
- `cargo test -p rocky-secure -p rocky-credentials -p rocky-sdk`: passed.
- Rerun with slice 3 on top: `sdks/kotlin/gradlew -p sdks/kotlin check`
  passed; `./examples/shoppinglist-app/check` ran 56 of 58 JVM tests green.
  Besides `SchemaUpgradeTest`, `ApprovalsTest` failed once with
  "coordinated write: Undecided" and passed on the slice 3 run.

### Open in slice 2

- The SDK signs folds only in its `compact` action; no SDK test covers a
  domain with existence logs, since the SDK test schemas declare none.
- An existence fold at a seal that later loses a race is not rebuilt; with
  the rows' compaction it waits for slice 3's rebootstrap. Slice 3 replaces
  it with a signed base at a seal of the path.

## Slice 3: checkpoints at seals and rebootstrap

See [checkpoints at seals](../runtime/lazy-compaction.md#checkpoints-at-seals)
and [rebootstrap](../runtime/lazy-compaction.md#rebootstrap). A checkpoint
record (`SignedCheckpointRecord`, format 1) is a tip statement naming a seal,
a scope, the checkpoint's digest, its signing node and its rows; the SDK lists
its bases after `compact`. With a tip graph, offers carry only bases at seals
of the followed path. Admitting a winning rival no longer fails: the device
follows it with its rows held still and writes refused until every scope has
a base on the path, from a listed checkpoint, its own frames or a peer's.

| Gate item | Test and result |
| --- | --- |
| With `keep = 0`, a winning rival rebootstraps by replaying frames | `a_seal_left_uncompacted_that_loses_is_rebuilt_and_a_compacted_one_rebootstraps` (`rocky-secure` `tests::compaction`): the rival is admitted, `rebootstrap` names the lost seal and a delete is refused; `replay_at_seals` rebuilds the three rows, compaction resumes at the winner and writes continue |
| ... and from a checkpoint listed in the graph | `a_compacted_seal_that_loses_rebootstraps_from_a_checkpoint_the_graph_lists`: B compacts at the winner and the owner lists its base; A imports exactly that checkpoint, its frame the winner left out stays as on B, A finalizes from the imported base, compacts at the winner and authors without forking its log |
| A pruned device replays frames from a peer | `a_pruned_device_whose_compacted_seal_loses_replays_frames_from_a_peer`: A pruned every frame; with none listed it resets and takes them again from B under an eviction guard |
| Inspection shows each seal's checkpoints with their signers | `compact_keeps_the_requested_seals_and_every_row_across_reopen` (`rocky-sdk` tips): each compacted seal lists one owner-listed checkpoint with its node and rows; Kotlin `TipsTest` decodes `TipCheckpoint`; `checkpoint_records_name_a_held_seal_and_revocation_unlists_them` (`rocky-credentials`) |

Fixed on the way:

- A compaction base counted the creations of rows dropped at the seal, so no
  device could import it; it now names those of its stored rows, and keeps
  their stubs.
- Importing a checkpoint reset the author tip below the device's own held
  frames, and the next write forked its log; held frames now count.
- `restored` cleared an eviction guard only for an explicit subscription; a
  full replica's held scope clears it too.

Formats: the statement set gains the checkpoint record. All of phase 15 is
unpushed, so link `rocky.link.v9` and replica file
`rocky.encrypted-replica.v25` stay; checkpoint format 14 is unchanged.

### Verification, 2026-10-16

- `./scripts/lint`: passed.
- `./scripts/check`: passed; 167 unit tests and 1001 integration tests within
  their budgets.
- `cargo test -p rocky-secure -p rocky-credentials -p rocky-sdk`: passed.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed.
- `./examples/shoppinglist-app/check`: 57 of 58 JVM tests passed; only
  `SchemaUpgradeTest` fails, as recorded for slice 1, so the Android tests
  and builds did not run.

### Open in slice 3

- A logged scope whose fold sits at the lost seal cannot replay frames; it
  needs a listed checkpoint and a signed existence base.
- Switching lines still refuses a scope pruned at another cut until the
  rebootstrap above has given it a base at a seal of the new path
  ([phase 14](14-seals-and-bookmarks-evidence.md#what-remains-before-phase-14-is-implemented)).
- `NativeSession::rebootstrap_at_seals` runs in every SDK sync pass, but no
  SDK test drives a lost race end to end across devices.
- A scope pruned at another cut before the graph is found by
  `seal_rebootstraps` and imports a listed checkpoint the same way; no test
  covers that case or an SDK join of a graph domain by frames.


## Slice 4: remove the mergeable horizon

Removed declarations, participants, acknowledgments, deadlines, guards,
competing statements and completed certificates across credentials, replica,
wire, SDK and Kotlin. Inserts carry no finalized basis or key-release proofs;
seal diffs retain the finalization behavior. Existence bases use only local or
signed seal folds, with neutral `ExistenceCut` and `ExistenceLog` type names;
the signed `rocky.existence-base.v1` field encoding stays unchanged.
`installed_existence_base` moved to `replica/existence/base.rs`.

Pruned log prefixes remain as `secure_logs.pruned_prefix`, `has_pruned_prefix`,
`pruned_page`, `behind_pruned`, `PrunedPage` and the checkpoint family's
`Pruned`/`PrunedLogs`. The range page's `behind` flag and `retains_history`
remain. `secure_compaction_live_creations` replaces the scratch table, and
the horizon's bootstrap and participant tables are gone. Coordinated-cut
fences and the off-path exception in `check_frontier` remain.

The trusted profile's separate pruning signal is also renamed:
`PrunedNotice`, `Catchup::BehindPruned`, `SyncState.pruned_prefix` and
`RetentionStatus.pruned_prefix`; its worker module is `pruning`. Its only
notice wire magic is `rocky.pruned.v1`. The retained-state JSON field is
`pruned_prefix`, so its file identity revision moves from 3 to 4, refusing
older files before configuration or schema writes. This preserves the trusted
pruned prefix and its existing tests; it removes none of its behavior.

Checkpoint format 15 removes both declaration fields; format 14 is refused.
All of phase 15 is unpushed, so link `rocky.link.v9`, replica file
`rocky.encrypted-replica.v25`, row frame 11 and witness envelope 1 stay current.
The removed existence attachment variant and changed native message family
have only the current encoding; there are no migrations or legacy readers.

Horizon-only tests are removed. Shared row-move, admission, chain retention,
coordinated-cut and generation tests keep their still-supported checks.
The SDK's old manifest-dropping scenario depended on explicit generation
switches followed by horizons; seal-based schema changes remain phase 17.
Core manifest cache tests remain, and SDK compaction now drops unreferenced
manifests after signing and listing its bases.

Gate evidence and final verification follow with slice 5. The existence
probe now uses 64 create/delete cycles (131 records before the fold and none
after it). Unchanged scopes reuse their prior seal checkpoint when no included
operation or finalization diff changed them; the regression checks identical
checkpoint bytes, no table observer event and a working session after compact.

## Slice 5: guides

The three owning guides now describe only seal compaction, seal-based existence
bases and format-15 checkpoints. Live runtime links use lazy compaction. The
former guide is a 21-line pointer, retained because phases 6 and 14 still link
it as a prerequisite or historical evidence; those plans are unchanged. The
roadmap README identifies the replacement. The owning guides are 183, 180 and
161 lines respectively. The module map names compaction and existence-base
ownership; the root README listed no declaration API.

The shopping qualification guide distinguishes current custody/move checks
from historical release qualification. The removed harness calls and activity
kind are gone. Seal qualification on the app remains phase 20. This does not
claim a new physical-phone run or restore phase 17's schema discovery.

### Completion gate recheck

This table rechecks every gate item after slices 4 and 5. The naming pass
also covers the separate trusted runtime and its desktop/benchmark consumers,
so the source scan has no storage API or wire format named horizon.

| Completion-gate item | Current test / check |
| --- | --- |
| 64 insert/delete cycles leave no deleted-row tombstones or stubs, and no existence records | `tests::compaction::compaction_at_seals_plateaus_under_insert_delete_churn`; `tests::existence::seals::a_holder_folds_the_existence_log_at_a_seal_and_a_child_holder_imports_its_signed_base` now uses 64 cycles |
| Compacted and uncompacted devices apply the same late frames through two continue seals | `tests::compaction::compacted_and_uncompacted_devices_apply_late_frames_across_two_seals_alike` |
| A child-only holder imports the issuer base and resolves the same orphans | The existence-base probe above, with pages of two and no parent row key |
| Changing one table leaves another base, observer and session untouched | `tests::compaction::changing_one_table_keeps_the_other_base_observer_and_session_untouched`: byte-identical checkpoint, no observer event, session reads after compaction |
| A logged final seal compacts immediately; keep=1 survives a winning rival; keep=0 rebootstraps from a listed checkpoint or frames | `a_seal_its_log_committed_becomes_final_and_beats_a_later_owner_seal`; `a_seal_left_uncompacted_that_loses_is_rebuilt_and_a_compacted_one_rebootstraps`; `a_compacted_seal_that_loses_rebootstraps_from_a_checkpoint_the_graph_lists`; `a_pruned_device_whose_compacted_seal_loses_replays_frames_from_a_peer` |
| Inspection shows seal checkpoints and signers | SDK `compact_keeps_the_requested_seals_and_every_row_across_reopen`; credentials `checkpoint_records_name_a_held_seal_and_revocation_unlists_them`; Kotlin `TipsTest` |
| No horizon code, format or SDK operation remains | `rg -n -i horizon crates sdks/kotlin .config/test-tiers.toml` has no matches; repository consumer scan finds no `SignedHorizon`, `horizon_pending`, `HorizonFinalized` or `"type":"horizon"`; `tests::formats::unsupported_replica_and_checkpoint_formats_refuse_before_writes` rejects format 14 |

Other retained boundaries: `a_tainted_checkpoint_guards_its_scope_until_a_clean_base_is_imported`
checks cut guards on writes, range/transfer serving and checkpoint imports;
`unconfigured_peers_stay_within_the_bound_and_the_least_recently_served_leave_first`
now checks bounded frontiers without participant credentials. Pruned-prefix
bootstrap and behind-peer tests remain in native and SDK checkpoint tests.
`the_previous_trusted_file_identity_is_refused_before_writes` verifies identity
3 is refused without a file write, and the trusted notice roundtrip test also
refuses an unknown version.

### Verification of slices 4 and 5

- `./scripts/lint`: passed.
- `./scripts/check`: passed; 165 unit and 962 integration tests, including
  the SDK without default features, within the tier budgets.
- After the final unchanged-base check also retained key-epoch/revocation
  refreshes, `cargo test -p rocky-secure tests::compaction::` passed all seven
  probes and `cargo clippy -p rocky-secure --all-targets --locked -- -D warnings`
  passed. The existence seal probes passed both tests, including 64 churn cycles.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed.
- Local SDK packages rebuilt with `publishToMavenLocal`; the standalone chaos
  harness passed `cargo check --manifest-path benchmarks/chaos/Cargo.toml --locked`.
- Both new changes pass `scripts/commitlint --jj`.
- `cargo test -p rocky-secure -p rocky-credentials -p rocky-sdk`: passed,
  including the 100,000-row migration (the migrations binary took 1,091 s
  while the workspace and SDK artifacts built). After the trusted naming pass,
  package tests also passed with that unchanged large case skipped
  (`-- --skip a_migration_of_100000_rows_converges_on_every_member`).
- `./examples/shoppinglist-app/check`: the required-schema comparison passed;
  the final run compiled shared JVM and Android code and ran all 58 JVM tests:
  56 passed. `SchemaUpgradeTest` fails as expected until phase 17.
  `ApprovalsTest` again failed with "coordinated write: Undecided"; it passed
  in the preceding run (57/58 JVM tests). Android tests and the desktop/Android
  app builds did not run because the script stopped at the JVM failures.
  Initial stale Maven-local SDK and extracted Android AAR caches were refreshed
  using the app README's documented procedure; the compilation error is resolved.
- Final workspace, package and Kotlin reruns after trusted-prefix renaming
  passed. The workspace rerun includes the new trusted-file identity regression;
  Kotlin's final check also ran its simulator tests successfully.
- All 35 added relative Markdown links and heading anchors resolve. The code
  scan has no horizon names across the Rust crates, Kotlin SDK or tier config.

Every completion-gate item above holds. Phase 15 is implemented with the
limits below.

### Remaining limits after phase 15

The earlier slices' open lists remain history. Unchanged scope bases are now
reused, and the declaration-removal and guide slices are complete. These
limitations remain:

- Change detection for constraint finalization is per table, not per partition;
  a waiting seal scans stored frame headers on each maintenance step.
- The SDK signs existence folds only in `compact`. Its existing schemas do
  not test logged tables, and no SDK test drives a losing seal race end to end.
- A logged scope whose off-path fold released records needs a listed checkpoint
  and signed existence base; frame replay alone cannot restore its records.
- A pre-graph arbitrary-cut base with pruned history is found by
  `seal_rebootstraps`, but that particular import path has no dedicated test.
- Graph-domain join falls back to frames until a peer has a seal checkpoint.
- Old SDK generation-discovery qualification is not restored; phase 17 owns it.
