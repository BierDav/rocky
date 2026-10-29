# Interactive first: prerequisite audit

## 2026-10-29: blocked before slice 1

Audited change `lzoqqkwn`, starting commit `adb115fa`, parent `cb66b19f`
(`chore: merge phase 02 partial interactive-first work`). The working copy
was clean. No runtime changes or implementation slices were attempted.

The prerequisite is features_12 phase 2, bounded holds. Its roadmap was
removed by `0cc1eb01`; its last merged implementation and evidence remain
readable with:

```sh
jj file show -r 1dc6522c docs/features_12/02-bounded-holds.md
jj file show -r 1dc6522c docs/features_12/02-bounded-holds-evidence.md
```

That brief says phase 2 remains in progress. Its final evidence section,
"Sizing implementation verification and remaining scope", explicitly leaves
shared metadata scans, blockers, placements, tip prerequisites, publication
progress, restoration/custody frontier, rebootstrap discovery, move-following
and SQL connection planning unfinished. Source confirms surviving gaps:

- `replica/blockers.rs`: public `EncryptedReplica::blockers` acquires one
  background guard for `Inner::blockers`. The latter visits every domain;
  `derive_readiness` visits all domain scopes and rebootstrap needs under
  that guard. There is no per-acquisition page bound or release/revalidation.
- `replica/placements.rs`: public `wanted_placements` holds one background
  guard through discovery over the followed path and readiness scopes.
  The output request limit does not bound the seals/scopes examined when
  no requests are produced. There is no page release/revalidation.
- `replica/lock.rs` still delegates both acquisition classes directly to
  Tokio's mutex. It records classes and work, but has no waiter count,
  deferral or starvation guard; the interactive phase is not already present.

Completed foundation work is present: the group leader releases between
transactions, receive transactions admit up to four ordinary one-row frames,
settle uses one work unit per acquisition, checkpoint storage has row/metadata
steps, and passive WAL checkpointing runs outside the replica lock. Existing
regressions cover committed-prefix retry, receive step bounds, maintenance
resume, pruning and page reclamation. These do not establish bounded holds
for the remaining scans or qualify L0–L4 interactive latency.

The execution contract requires verification before editing and says to stop
and report a mismatch with the settled design. The prerequisite's settled
scan design requires bounded pages and validation before acting on a result.
Proceeding here would leave that foundation unmet. Resume this phase after
the surviving bounded-holds work is completed and verified against code;
start with slice 1, without treating this audit as implementation evidence.

No format changes, timing measurements, simulation changes or device runs
were made. Desktop/phone latency and paired sync gates remain unverified.

## Verification

- `./scripts/lint`: passed.
- `scripts/commitlint --jj`: passed.
- `./scripts/check`: passed formatting, workspace Clippy, 207 unit tests
  and 1,466 integration tests (1,142 workspace and 324 SDK without default
  features). Integration wall time was 246.94 s within its 300 s budget;
  the script's isolated checks also passed the per-test budgets. The host
  was heavily loaded during compilation and the workspace run.

The existing fixed-seed simulation tests are included in the integration
run. The e2e sweep, release measurements, 100k migration and device checks
were not run. No interactive probe was added, so these baseline results do
not satisfy slices 5–6 or any interactive completion gate.

## 2026-10-29: prerequisite completion and slices 1–6

Change `prpmqutm`, on `bfcbb026` (main). Phase start for pairing:
`d059fa48` (`uoqrnuuw`), the previous main.

### Bounded-holds prerequisite

The surviving features_12 phase 2 holders are now bounded or classified.

| Holder | Now | Test |
| --- | --- | --- |
| `domain_headers` callers: public `seal_frontier`, `earlier_epoch_writes`, `tip_observation`, `tip_nodes` | `replica/headers.rs`: keyset pages of `HEADER_ROWS` (256) over operations, voids and optionally held frames, each its own background acquisition, validated against both tables, authority, tip, context epoch and graph shape | `header_scans_page_their_reads_and_restart_on_a_changed_source` |
| required schemas, missing live schemas, schema retention, migration requirement lookups, `install_tip_schema` | same scan; the acting step (store, drop, list) runs in the acquisition that validated it | schema, tip-schema and migration suites |
| public `blockers()` | one domain, one state component or `PENDING_HEADS` (64) pending log heads per acquisition, validated against the readiness state tables plus quarantine, dropped-tip, scope-state, position and log tables | `paged_pending_blockers_add_up_across_authors` |
| `wanted_placements` | one planning acquisition, then one (seal, scope) check per acquisition; `unverified_scopes` hoisted out of the loop | placement suite |
| publication progress | one publication or one bundle check per acquisition; each advance commits at once (`published` only grows) | publication suite |
| rebootstrap discovery | per-scope predicate; public discovery one scope per acquisition; `replay_at_seals` checks only its own scope | rebootstrap and recovery suites |
| `replica_status` | one scope per acquisition | roles suite |
| move-following | a foreign commit drops only the current row; the cursor survives, and a pass reruns once when moved rows changed behind it (previously any commit restarted the pass, so it never finished under steady writes) | row-move suites |

A paged scan whose source keeps changing restarts; after `SCAN_PASSES` (3)
refused passes it runs once in a single background acquisition, counted
as `scan_fallback`. Like a transition, that step starts only when no
interactive write waits. Without it, steady writes starved the scans
(global-counter starvation, as found before for checkpoint preflight).

Classified, not paged:

- In-transaction uses (partition creation, tip and schema-stage admission,
  `switch_tip`, migration publication admission) keep the unpaged scan
  inside their transition.
- Restoration completion runs only while an eviction guard exists: a
  transition.
- Remaining secondary dimensions: per-domain readiness derivation
  (O(scopes × position runs), on the gate path after each commit), the
  custody frontier (O(range rows)) and reader refresh after a commit
  (O(readers × tables × partitions)). Making reader refresh lazy was tried
  and reverted: cursors must not wait for the writer lease
  (`cursor_readiness_and_snapshot_do_not_wait_for_the_writer_lease`).

### Slices 1–4: the gate

`replica/lock.rs`:

- Interactive acquisitions register before waiting and unregister on
  acquisition or cancellation.
- A background acquisition waits for zero registered waiters and
  rechecks after it gets the mutex.
- `STARVATION_YIELDS` = 8 interactive acquisitions per deferred
  background acquisition. The guard is per acquisition, so a bounded
  loop's next step behaves the same way.
- No clocks are used.

Every bounded loop already releases between steps, so step-boundary
yields come from the background re-acquisition. Transitions, the fan-out
exception and fallback scans acquire as background, so they start only
when the gate is quiet. The group leader re-acquires as interactive while
its remaining cohort holds a local write.

Tests:
- `background_queued_first_acquires_after_interactive`
- `cancelled_interactive_waiter_unregisters`
- `interactive_acquisitions_stay_fifo`
- `starvation_guard_lets_background_advance`: the background acquires
  after exactly 8 interactive acquisitions.
- `held_listing_bounds_current_rows_and_refuses_a_changed_source`: an
  interactive waiter registered between listing steps acquires before
  the next step.

Test support records `interactive_steps`: the background steps each
interactive acquisition waited for.

### Slice 5: simulation

Schedules gain `Probe`: a write racing the node's own sync pass. The
fixed seeds 1–8 and all regressions pass (`cargo test -p rocky-sdk --lib
simulation`, 22 passed); `KNOWN_LIVENESS` stays empty.

### Slice 6: desktop L0–L4

- Host: Apple M5 Pro, 18 cores.
- Command: `cargo test --release -p rocky-sdk --features qualification
  --test migrations write_probe -- --ignored --nocapture
  --test-threads=1`.
- Samples: 600 per mode after 50 warm-up, one every 100 ms; 100k-row
  fixture.
- Candidate ran with load average 13–16 at start falling to 4; baseline
  `d059fa48` ran with 3–6.

Write latency (ms, p50 / p95 / p99 / max), baseline → candidate:

| Load | Mode | Baseline | Candidate | Candidate reactor delay p95 |
| --- | --- | --- | --- | --- |
| L0 | Flushed | 8.4 / 10.0 / 12.3 / 15.2 | 6.1 / 9.3 / 15.8 / 21.1 | 0.3 |
| L0 | Buffered | 4.8 / 6.2 / 8.6 / 10.4 | 4.0 / 5.6 / 9.0 / 15.9 | 4.5 |
| L1 | Flushed | 12.5 / 166.2 / 212.9 / 329.9 | 13.0 / 187.6 / 332.7 / 6423.8 | 56.3 |
| L1 | Buffered | 9.0 / 188.3 / 235.7 / 312.4 | 9.0 / 180.9 / 222.9 / 263.5 | 176.7 |
| L2 | Flushed | 17.8 / 51.8 / 70.6 / 110.8 | 20.5 / 54.4 / 66.5 / 152.4 | 35.9 |
| L2 | Buffered | 11.4 / 45.3 / 61.1 / 171.2 | 11.2 / 38.5 / 49.6 / 65.7 | 37.5 |
| L3 | Flushed | 7.7 / 11.1 / 13.8 / 29452.2 | 9.7 / 11.7 / 14.2 / 28252.9 | 0.8 |
| L3 | Buffered | 2.9 / 3.6 / 6.9 / 28046.2 | 6.0 / 7.6 / 8.7 / 34626.9 | 6.0 |
| L4 | Flushed | 15.4 / 178.2 / 192.5 / 196.9 | 15.6 / 177.5 / 196.8 / 252.1 | 162.7 |
| L4 | Buffered | 10.2 / 169.7 / 178.3 / 244.6 | 12.1 / 169.8 / 185.1 / 188.8 | 166.2 |

Replica lock waits of the probe stay at most 0.013 ms at p99, and in every
scenario no interactive acquisition waited for more than one background
step (`over_one_step=0`; the recorded step count was 0 throughout).

**The interactive latency target is not met, and the gate cannot meet
it.** The time is spent queued on the SDK reactor thread
(`write_reactor_delay`), before the write reaches the replica lock.
Receive, maintenance and checkpoint calls run on that thread, so a write
waits behind them there, not at the lock. Each L3 maximum is a
28–35 s reactor stall, present in the baseline too.

Paired with the baseline, the candidate is equal within noise: L1, L2
and L4 p95 match, and the L3 Buffered p95 rise coincides with the
candidate's higher host load. Meeting the 16/50/100 ms target needs
interactive priority on the reactor, which is outside this brief
("priorities among background holders" and reactor scheduling are not
in scope). Recorded unmet, not relaxed.

Not run:
- phone measurements;
- the paired 100k member pull with the probe;
- the e2e sweep.

`tests::roles::a_far_behind_replica_catches_up_while_both_sides_keep_writing`
hangs for more than 7 minutes on the unchanged baseline `d059fa48` too.
It is pre-existing and is not addressed here.

### Follow-up fixes (2026-10-29)

- The settle-pass livelock behind
  `a_far_behind_replica_catches_up_while_both_sides_keep_writing` is fixed
  below this change by `fix(secure): keep the pending pass across local
  authoring commits` (linz, `5d9473ca`). The e2e test takes about 6.4 s
  again.
- Paged scans no longer validate against the context epoch. Every SQL
  session bumps it when it drops, even after a plain commit, so each SQL
  write restarted them. They still validate against the tables they read,
  the authority, the tip and the domain graph.
- A placement listing whose authority or tip moved returns what it has
  instead of failing the peer's sync session. The receiver revalidates
  each request.
- `readiness_after` reads the status clock before computing the status and
  anchors its expiry wake to that instant. Otherwise an expiry while the
  status was computed left the wait asleep forever
  (`expiry_alone_wakes_a_status_wait_and_observation_does_not_start_sync`).
- `a_stalled_bootstrap_peer_does_not_hold_the_checkpoint_import_slot` hung
  in about half of its runs when the publication listing yielded once
  before its first acquisition. Without that yield it passed 12 of 12;
  the linz base passed 10 of 10.
  - The test or the SDK sync pass depends on where a scheduling point
    falls relative to a clock advance on the paused clock. One extra yield
    before the stalled dial's timer leaves that timer unarmed when the
    test advances.
  - Only the first-yield trigger is fixed here; the ordering dependency
    itself remains open.
