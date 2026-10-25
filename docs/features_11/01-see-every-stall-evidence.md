# 1. See every stall: evidence

Status: implemented 2026-10-25 on top of `746c9afb`. The overhead and manual
device gates remain open (see [Not verified](#not-verified)).

## What landed

- **Blockers.** `rocky_core::Blocker { scope, kind, clears_by, since }`
  with `BlockerKind`, `ClearsBy`, `WaitReason` and `FrameReplay`, plus
  `scope_label` / `domain_label` (12 hex characters of a hash).
  `EncryptedReplica::blockers()` derives them from stored state
  (`rocky-secure/src/replica/blockers.rs`). Beyond the brief's list, the
  closed enum has `LineNode` (the followed line has no node of this
  identity). That was the "line signers" refusal, which `author_frame` and
  `ensure_tip_writable` used to raise as two separate plain strings.
- **One source.** `require_workspace_base`, `require_scope_readable`, the
  dropped-tip check that replaced `ensure_tip_restored` and the line-node
  check are now `refuse(<blocker list>)`. Authoring refuses with all of
  them together. `table_complete`, the bootstrap/identity-allocation import
  check, `has_pending` and `importing` are the same predicates that
  `blockers()` reads. A refusal is `Error::Blocked(Vec<Blocker>)`. The SDK
  reports it as code `blocked`, retryable, with a `blockers` array. The
  message starts `the workspace waits for N blockers:` and lists each
  blocker. RockyRoom's transient-read match follows that prefix.
- **Wait reasons.** `Outcome::Waiting(WaitReason)` at every waiting return
  (schema, direct import, opening key, read grant, own chain, dependencies,
  undecryptable, plus ledger). A failing head is recorded as `Failed`. The
  apply pass keeps the reason of every pending log head in memory, removes
  entries for heads that applied, and prunes the map when the pass settles.
  Frames behind a head are counted under its reason. Test builds check,
  at every settled pass in the whole suite, that each pending head has a
  reason. Nothing is persisted, and no format moves.
- **Status.** `DataStatus.blockers` (the replica's blockers, plus the
  SDK's `BootstrapMarker` per intent scope and `AwaitingInviter` naming the
  inviter; `since` is the marker file's mtime). `bootstrap_pending` stays.
  `Peer.last_sync` keeps each peer's last exchange that was not skipped.
  `PeerReconciliation` gains per-scope `scopes` outcomes (synced, refused,
  peer importing, failed) and `timeouts` by kind. Transfers report
  `pulled`/`pushed` frames and `received_bytes`/`sent_bytes`. The native
  build script also exports `rocky-core/src/blocker.rs`, so Kotlin gets
  `Blocker`, `BlockerKind`, `ClearsBy`, `WaitReason` and `FrameReplay` as
  status typealiases.
- **Spans.** `tracing` is a dependency of rocky-secure, rocky-sdk,
  rocky-iroh and rocky-coordinator. None of them installs a subscriber.
  Spans:
  `sync_pass`, `exchange`, `dial`, `iroh_connect`, `admission`, `ready`,
  `session`, `request` (channel, family, bytes, duration_ms, outcome),
  `serve`, `page_apply` (scope, frames, applied), `apply` (frames, rows,
  waiting), `checkpoint_page`, `finalize`, `compact`, `maintain`,
  `coordinated_call` (append/acknowledge), `coordinated_resolve`, and in
  the coordinator `coordinated` (action) and the voter's `resolve` and
  `acknowledge`. Field values are labels, counts, durations and
  `Error::category()` / `CoordinationError::category()`. Finalization
  reasons now name scopes by label instead of a debug dump.
- **Event sink.** The native binding's `set_event_listener(listener,
  level)` and `dropped_events()` install one registry subscriber on first
  use. Closed spans and events are rendered and pass through a
  1,024-entry `sync_channel` to a `rocky-events` thread. `try_send`
  failures are counted. The level filter is applied at the callsite
  (`Interest::never` while no listener is set). Kotlin exposes this as
  `RockyDiagnostics.listen(level, listener)` and
  `RockyDiagnostics.dropped`. The shopping app's `observeDiagnostics`
  (desktop and Android) writes `rocky <rendered span>` lines at DEBUG to
  its rotated `diagnostics.log`, and its status line now names blockers.
- **Bindgen fix.** The Kotlin/Native renderer compared a type's full module
  path with the crate name. A record or enum from a submodule, passed by
  value, was then treated as external, which broke the iOS bindings for
  `NativeEvent`. It now compares crate names.

## Completion gate

| Gate | Result |
| --- | --- |
| Reset replay | `tests::compaction::a_pruned_device_whose_compacted_seal_loses_replays_frames_from_a_peer`: after `replay_at_seals(items, reset)`, `blockers()` names items `RestorationGuard` and `SealRebootstrap { replay: Peers }`, both cleared by any peer, along with the domain's `LostCompaction`. The SQL insert is refused with `Error::Blocked` naming items' `SealRebootstrap`, and every refused blocker is in `blockers()`. After syncing from B, no restoration, rebootstrap, lost-compaction or pending blocker is left. |
| Import intent | `tests::partitions::a_future_import_does_not_make_verified_empty_partition_history_missing`: `blockers()` names scope B `Importing`, cleared by any peer, and lists no `SealRebootstrap`. Writes to A are not refused, which is the fixed behaviour, so there is no refusal to inspect. |
| Line signers | `tests::tips::reopening_a_branch_waits_for_its_saved_line_node_and_returns_to_main`: after reopening on the branch, the write is refused with `LineNode` cleared by local work, `blockers()` names the same blocker, and it clears once the saved line node is registered. |
| Unshared proof | `tests::checkpoint::no_frames::seal_recovery_skips_a_peer_outside_the_missing_scope`: the device missing the private scope's verified state has a `SealRebootstrap` for it, cleared by any permitted peer, in `blockers()`, and its write is refused with that blocker. A session with a peer that does not share the scope leaves the blocker named and unchanged. `network::holdings::tests::an_unshared_partition_does_not_end_held_recovery_of_other_scopes` still passes; its stall was a session ending, which phase 3 owns. |
| Unused partition | `rocky-sdk::shared_lists::an_unused_own_partition_prepared_before_a_seal_remains_creatable`: after seal, compaction and sync, status lists no `seal_rebootstrap` blocker and the prepared partition is creatable. |
| Own-id preparation | `rocky-sdk::shared_lists::preparing_an_own_id_does_not_claim_an_unrelated_group_members_state`: the refused insert names `seal rebootstrap (replay=…)`. Status lists `seal_rebootstrap` blockers cleared by `any_peer`, and each status blocker's scope label appears in the refusal. |
| No pending frame without a reason | `tests::tips::frontiers::headers::a_pending_frame_names_why_it_waits_until_it_applies`: a bundle member waiting for its sibling is `PendingFrames { Dependencies, 1 }`, cleared by any peer, and nothing is pending once the sibling arrives. In test builds every settled apply pass fails with `Corrupt` if any pending head lacks a reason. The full rocky-secure suite ran with that check on. |
| Span tree without data | `rocky-sdk::spans::a_sync_pass_is_a_span_tree_without_application_data` installs a recording `tracing-subscriber` layer and syncs a joined device. It finds `exchange` under `sync_pass`; `dial`, `admission`, `ready` and `session` under `exchange`; `request` under `session` (a `rows` request with channel, bytes, duration_ms and outcome); and `page_apply` under `sync_pass`. A scan of every recorded field finds neither the row title, the invitation URI nor any of its ≥ 12-character parts, nor any file in either device's `secrets` directory as raw text, hex or byte list. |
| Native and Kotlin sink | `rocky-uniffi events::tests::closed_spans_reach_the_listener_off_thread_and_overflow_is_counted`: delivery on `rocky-events`, the level filter, the span path, and a blocked listener overflowing into `dropped_events()`. Kotlin `DiagnosticsTest`: a join plus `syncOnce` delivers `sync_pass` and a successful `admission`, and no rendered event contains the invitation. |
| 100k catch-up within 3% | See [Overhead](#overhead). |
| Shopping diagnostics on devices | Not verified: no manual device sync was run. The desktop and Android builds compile with the sink. |

## Overhead

`a_migration_of_100000_rows_converges_on_every_member` (release test
binaries of `746c9afb` and of this change, no subscriber installed),
`member_pull` per run:

| Run | Base `746c9afb` | This change |
| --- | --- | --- |
| 1 | 37.07 s | 39.74 s |
| 2 | 28.40 s | 44.76 s |

Not verified. The machine was shared with another workspace compiling and
running `rocky-sdk` tests (load average 44–69 on 18 cores), so runs of the
same binary differ by 30 % or more. These numbers cannot confirm or
refute the 3 % gate. The idle-host qualification below did not reach the
measured stage either.

### Mac mini qualification, 2026-10-25

The continuation of T3 thread `29f3266a-8595-4cd0-966c-de9fcad57282`
used an Apple M4 Mac mini (`Mac16,10`, 10 physical cores, 16 GiB RAM),
macOS 27.0.1 (26A434), Rust 1.99.0 (`b940084d7`), and the saved release
test binaries of `746c9afb` and `3ed4ad8f`. No subscriber was installed.
Each binary was invoked directly, alone, with the fixture's existing
600-second timeout:

```sh
<migrations-binary> --exact \
  a_migration_of_100000_rows_converges_on_every_member \
  --nocapture --test-threads=1
```

| Revision / storage | Seed open | Additive open | Result |
| --- | --- | --- | --- |
| Base `746c9afb`, SSD, prior thread | 172.533 s | 17.199 s | Timed out at 600 s before `fill_open` |
| Base `746c9afb`, 2 GiB APFS RAM disk, prior thread | 183.029 s | 16.934 s | Timed out at 600 s before `fill_open` |
| Current `3ed4ad8f`, SSD, continuation | 175.278 s | 17.020 s | Timed out at 600 s before `fill_open` |

The earlier current SSD attempt was interrupted after 152 seconds; it is
not a timeout or a timing sample. The continuation's current attempt ran
for 600.049 seconds, with load averages 2.28 / 2.03 / 1.98 before and
2.49 / 2.45 / 2.24 after. No other builds or checks ran during it.
A one-second process sample during preparation showed active SQLite
execution. The prior baseline sample included `seal_drops`; these samples
do not establish the cause of the slow preparation.

None of these attempts reached `member_pull`, so there is no new overhead
percentage. The RAM-disk attempt also failed to reach catch-up. The 3 %
gate remains **unverified**, blocked by full-size fixture preparation
exceeding its budget on this host. Neither the fixture nor its timeout
was changed to obtain a result.

## Checks

### Implementation-machine checks

- `./scripts/lint`: pass. `cargo clippy --workspace --all-targets -D
  warnings`: clean.
- `./scripts/check`: unit tier 192 passed; integration tier 1,311 passed
  and 1 failed. The failure,
  `tests::tips::frontiers::headers::excluded_and_other_domain_headers_still_decode_after_warming`,
  fails the same way on `746c9afb`.
- Full `rocky-secure` nextest run: 533 passed. Two e2e-tier tests time out
  under plain nextest on both `746c9afb` and this change.
- `rocky-sdk` `shared_lists`, `spans`, `bootstrap` and lib tests: 145
  passed.
- `sdks/native/check`, `sdks/kotlin/gradlew -p sdks/kotlin check`
  (including `DiagnosticsTest`) and `./examples/shoppinglist-app/check`:
  pass.

### Mac mini continuation

- `./scripts/lint`: pass.
- `CARGO_BUILD_JOBS=2 ./scripts/check`: clippy clean; unit tier 196
  passed, including the SDK without default features. Integration: 1,322
  passed and one failed, with the same named header-decoding failure
  recorded above (`expected rejection "invalid secure frame", got success`).
  All completion-gate blocker regressions and the span/privacy test passed.
  Integration suite time was 206.87 s within its 300 s budget, but six
  distinct tests exceeded their two-second budgets on isolated reruns
  (nine violations across the two feature builds). The command exits 1.
- `sdks/kotlin/gradlew -p sdks/kotlin :rocky-core:jvmTest --tests
  rocky.DiagnosticsTest`: pass. This reruns the real join and explicit
  `syncOnce` through the native listener; it is not a manual shopping-app
  device check.
- The shopping desktop app was not run: its SDK was absent from Maven
  local and the full multiplatform publication, which also builds iOS
  runtimes and metadata, was not completed. The direct JVM test above
  does not qualify the packaged SDK or change the earlier app-build evidence.

Isolated integration budget violations (a dash means no violation in that
build, not that the test was necessarily present):

| Test | Workspace | SDK without defaults |
| --- | --- | --- |
| `rocky-sdk node::revocation_tests::reopen_completes_each_revocation_secret_boundary` | 2.61 s | 2.41 s |
| `rocky-sdk::tips races::revoking_the_identity_whose_seal_won_makes_every_device_follow_the_next_winner` | 3.06 s | 3.05 s |
| `rocky-sdk::tips trusted::a_trusted_identity_opens_a_branch_that_devices_on_main_never_see` | 2.19 s | 2.12 s |
| `rocky-secure tests::existence::seals::a_holder_folds_the_existence_log_at_a_seal_and_a_child_holder_imports_its_signed_base` | 2.08 s | — |
| `rocky-secure tests::formats::unsupported_replica_and_checkpoint_formats_refuse_before_writes` | 2.23 s | — |
| `rocky-secure tests::kept::five_kept_seals_of_one_percent_changes_add_about_five_percent` | 2.22 s | — |

These timing violations were not requalified against the baseline. No
budgets or tests were changed, and no e2e tier beyond the full-size
migration qualification above was run in this continuation.

## Not verified

- The 100k catch-up overhead gate (see [Overhead](#overhead)): preparation
  times out before catch-up on this Mac mini, including the baseline on
  a RAM disk. A valid comparison still needs both revisions to finish
  preparation within the fixture's existing budget.

- The shopping app's diagnostics file was not inspected after a manual
  sync on desktop or Android. On this Mac mini, `device_list` reports
  `Android SDK was not found` and device support failed during installation;
  it exposes no devices. No manual desktop sync was performed either.
- The held-recovery "unshared partition" regression has no stored blocked
  state, so no blocker assertion was added to it.
