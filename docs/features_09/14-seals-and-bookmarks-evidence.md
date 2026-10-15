# Phase 14 evidence

Lanes record their evidence in their own sections; the orchestrator merges
them and closes the phase.

## Lane S — seals, tips, bookmarks and late frames

### Slice 1: statements and formats

`rocky-credentials` defines signed seals, tip openings and bookmark moves at
format 1. `TipGraph` resolves concurrent seals and same-source moves by lower
signed digest, retains structurally valid losing histories, and redirects a
losing continuation to its canonical successor. Until phase 17 a continue seal
must preserve its parent's manifest digest. Genesis is
`commitment("rocky.genesis-tip.v1", manifest_digest)` wrapped in the shared
`rocky_core::TipId`; the graph's `Scope` qualifies that identity.

The capability extension is version 5. `Extended.trusted: Option<Scope>` is an
owner-direct grant to an identity; a node never inherits control authority.
`Authority::issue_trusted` issues this grant. The coordinator's trusted list
can later derive membership from these verified credentials; this lane does
not alter coordinator control certificates.

Row frame 10, existence record 4, checkpoint 13, encrypted file identity 23,
mergeable horizon 7 and link handshake 7 carry tips. Previous versions are
refused. Row frames also sign `author_sequence`, a transactionally reserved
position across the node's resource logs, because their existing p2panda
sequences count independently per scope. Seal frontier marks use this global
position, not the p2panda sequence.

Verification during implementation: `cargo test -p rocky-credentials --locked`
passed (36 unit, 13 credentials, 5 identities, 6 tip tests).
`cargo test -p rocky-secure --lib formats --locked` passed the three format
refusal tests before the global author-position assertion was added; the
final verification below records the complete run.

### Slice 2: following and admission

Durable `secure_tip_graphs`, `secure_tip_statements`, `secure_followed_tip` and
`secure_line_nodes` retain the verified graph and workspace selection. Admission
exchanges the full digest-sorted union before refreshing followed-tip claims; request sessions refresh the graph on
already-open links as well, before SDK row reconciliation;
dependencies may cross page boundaries. Graph persistence and any bookmark
workspace rebuild commit together. Unknown/evicted peer tip claims fail closed
once graph records exist. Unrelated-tip frame receipt, row offers, ranges,
frontiers, checkpoints and remote reads are refused or filtered.

Replica APIs: `followed_tip`, `followed_bookmark`, `tip_graph`, `tip_statements`,
`receive_tip_statements`, `publish_tip_statements`, `follow_bookmark`,
`enroll_line_node`, `register_line_node`, and `drop_tip`. The caller retains
line-node secrets and supplies them after reopen; no private key is written to
protocol tables. Switching back selects the saved node. `secure_node_tips` binds each node's last
authored tip, so a losing continuation cannot reuse its node across the fork. Dropping an inactive
workspace's frames preserves graph, node/sequence reservations and handles,
requires custody for local writes and refuses retained dependencies/checkpoints
or held evidence. Re-following waits for original dropped frames before writing.

SDK discovery no longer runs `adopt_newer`/`learn_older`, carries a `newer` flag
or uses sync `upgrade` state. Explicit schema opens remain the pre-phase-17
mechanism; no graph-backed schema change is accepted yet.

### Slice 3: late and held frames

Late frames walk continue seals without changing their author's bytes. Close
and winning backward/sideways bookmark moves hold excluded writes; losing seal
continuations also retain their writes as held evidence. Branch openings only
inherit their sealed prefix. Replay preserves inherited snapshot frames even
when another line classified them as held.

`secure_held_frames` retains original encrypted frames and
`secure_held_resolved` records verified resolution authority. Readable holders
serve original frames across tips; `NativeSession::{pull_held_page,
push_held_page,sync_held_changes}` provide bounded transfer. Native scope sync
and SDK sync exchange held frames when a graph exists. Bundle body format 5
adds the signed re-authoring ID, so one re-authoring removes held state on each
receiver; format 4 is refused. This is independent of row frame format 10.

### Integration contracts and remaining qualification

Lane L keys logs by `(Scope, TipId)` on this `followed_tip()`; the
[integration](#integration-terminal-seals-in-the-graph) replaced the refusal
of coordinated seals and moves with terminal log entries. SDK/JSON/Kotlin
seal/follow/status/inspection surfaces remain wave 2.

A switch that requires pruned history refuses instead of inventing a branch
base; importing a target checkpoint during follow remains integration work.
Legacy explicit schema upgrades with unsent old-tip frames can leave later author
positions unavailable to a peer or a fresh join incomplete. Phase 17's sealed
schema-change contract must resolve this; tests of discovery refusal and local
pending-work preservation do not establish cross-tip delivery of those frames.

Final command results are recorded below after verification.

### Targeted verification

After fixing the resumed build and regressions:

- `cargo test -p rocky-secure --lib --locked tip`: 23 passed, including
  two-device branch isolation/drop/return, inherited held replay, two continues,
  close and signed re-authoring, rollback convergence, concurrent seals with
  divergent writes, graph paging and refresh over an already-open link.
- The SDK removal/re-add regression passed after retaining every device's base
  before its explicit schema switch and writing the replacement value on the
  followed tip; unfollowed old-tip frames are not offered as a substitute.
- Repeated and empty graph receipt leave authority unchanged, so a no-op graph
  refresh never closes healthy live links.

The row-cost test passed with 10 and 10,000 stored rows: no row bytes changed
and SQLite metadata write counts matched. Its isolated integration-budget
confirmation was 683 ms on the shared host. This is a deterministic work-count
regression, not a claim of identical wall-clock measurements.

### Final workspace verification

`./scripts/lint` passed. `./scripts/check` passed workspace Clippy, 167 unit
checks and 938 integration checks: 697 workspace tests plus 241 SDK tests without
default features. Both tier suites and their per-test confirmations met their
budgets; the integration suite took 134.58 seconds. The existing re-add test's
3-second exception was retained, with its updated name; its isolated confirmation
was 1.87 seconds. No new budget exception was added.

The earlier authority-invalidation, cross-tip offer and fixture-close failures
were corrected and passed in this final run. E2E, Kotlin/Android/iOS and native
packaging qualification were not run; no Kotlin or native ABI source changed.
These checks qualify this lane's Rust behavior, not the remaining coordinated
terminal-entry integration, SDK developer surfaces or phase 17 schema changes.

## Lane L: one coordinated log per tip (slice 4)

Guides: [coordinated log](../runtime/coordinated-log.md),
[coordinated writes](../runtime/coordinated-writes.md),
[coordinator retention](../runtime/coordinator-retention.md),
[public coordination](../runtime/public-coordination.md) and
[stuck recovery](../runtime/stuck-recovery.md).

### Contracts

- `rocky_core::TipLog { scope, tip: TipId }` identifies every coordinated
  log: coordinator state and requests, the `DomainLog` port, rocky-secure
  storage (`secure_coordinated`, `secure_entries`, `secure_cuts`,
  `secure_entry_frames`, `secure_cut_submissions`), `Slot.log`,
  `CoordinatedEntry.log`, `EntryPeers::committed`, `entry_hash`, the entry
  wire and `CutStatement.log`. `rocky_core::log_genesis(log, configuration)`
  anchors each tip's log.
- `DomainLog` gains `configure(log, minimum)` and `seal(log, LogTerminal)`;
  `LogHead` gains `terminal: Option<LogTerminal>`;
  `LogTerminal { digest, continuation: Option<TipId> }` carries the opaque
  digest of a seal or a bookmark move and the continuation tip.
  `CoordinationError::Sealed` refuses everything after a terminal entry.
- Coordinator: `Operation::Coordinated { log: TipLog, action }`,
  `Operation::Configure { scope, minimum }` for the domain's default minimum
  (`NetworkCoordinator::configure`), `Action::Configure` per tip by the
  authority or a trusted node before the first join, `Action::Seal { terminal }`
  by the authority or a trusted node. `TIP_LOGS = 8` open logs per domain
  (`Backpressure` beyond), `ENDED_LOGS = 8` retained ended ones (earliest
  dropped). A node is in sync on one open tip per domain; joining another
  leaves the first, and waits with `Backpressure` while it has a pending
  entry.
  `MemoryLog::configure(scope, minimum)` sets the default; `MemoryLog::head`
  takes a `TipLog`.
- rocky-secure: lane S's `followed_tip()` decides every log;
  `followed_log(domain)`, `seal_log(log, terminal, coordinator, peers)`;
  `CoordinatedError::{Unfollowed, Sealed}`; entry state `carried`; the
  internal `Inner::tip_log(domain)` used by cut fencing, checkpoint bindings
  and cut sync in `bundle.rs`, `roles.rs`, `tips.rs`, `checkpoint/create.rs` and
  `network/rows.rs`. `held.rs`: `void_bundle(transaction, hold)` now holds
  the voided bundle itself when `hold`.
- SDK: JSON codes `unfollowed_tip` and `sealed` (not retryable);
  `coordinated` status adds `tip` (hex). `Node::propose` takes the slot's
  log.
- Forwarding: no coordinated command forwarding exists (phase 11 removed it;
  only enrollment forwarding remains, which carries no coordinated write).
  Nothing routes one tip's write elsewhere: slots, stores and member
  recovery serve only the followed tip's log, and a store of another tip's
  entry is refused `Unfollowed` before anything is kept or acknowledged.

### Formats

| Format | Version |
| --- | --- |
| Coordinator store and snapshot | 8 |
| Coordinator request signature | `rocky.coordination-request.v6` |
| Cut statement signature | `rocky.coordinated-cut.v2` |
| Entry hash | `rocky.coordinated-entry.v2` |
| Log genesis anchor (new) | `rocky.coordinated-genesis.v1` |
| Encrypted replica file identity | `rocky.encrypted-replica.v23`, shared with lane S |
| Live link handshake | `rocky.link.v7`, shared with lane S |

Lane S moved the replica file and link to the same versions; the merged
result bumps each once.

### Completion-gate tests

| Gate | Tests |
| --- | --- |
| A write on `dev` commits through `dev`'s log; `main`'s set sees none | `rocky-coordinator machine::tip_tests::a_tips_writes_commit_through_its_own_log_and_followers`; `rocky-secure tests::coordinated::tips::a_dev_tips_write_commits_through_its_own_log_and_main_sees_no_entry`; `rocky-sdk::coordinated` checks the followed tip's log holds all six writes and another tip's none |
| Committed before a seal is part of the continuation; a pending one retries there once | `tests::coordinated::tips::a_pending_write_at_a_seal_retries_in_the_continuation_and_commits_once` (the proposer reopens at generation 4, the continuation, and its write commits as entry 1 there) |
| After a terminal entry the log refuses appends | `machine::tip_tests::a_terminal_entry_aborts_the_pending_one_and_ends_the_log`; the secure seal test above (`Sealed` slots, repeated and diverging seals) |
| A pending write at a `close` is held | `tests::coordinated::tips::a_pending_write_at_a_close_is_rolled_back_and_held` |
| A tip no trusted node follows refuses coordinated writes | `machine::tip_tests::a_tip_no_trusted_node_follows_refuses_writes_and_logs_are_bounded`; `Unfollowed` in the dev tip test |
| Bounded logs | the test above (open logs) and `machine::tip_tests::ended_logs_are_retained_up_to_their_bound_and_open_none` |

### Verification

`./scripts/lint` and `./scripts/check` pass on 2026-10-15: 167 unit tests
(budget 10 s) and 918 integration tests in 139 s (budget 300 s). In that
loaded run the rocky-secure tip tests took 1.2–1.5 s each (budget 2 s) and
the coordinator's `tip_tests` 12–13 ms, which the integration filter now
names. Kotlin and the shopping check were not run; this lane changes no
Kotlin code.

### Limits

Lane L's limits before the integration are superseded by the
[integration's](#limits-after-the-integration).

## Integration: terminal seals in the graph

Wave 2 of lane L, on lane S's `znpwrnrx`: `ssrouqpz` (seals end their
tip's log), `spynwwxw` (leaving a tip's log), `krqynxux` (continuations by
the tips frames name) and `vknrxyyx` (gate tests). The rebase bumps the
replica file (v23) and link (v7) once, and lane L's stand-in `followed_tip`
is gone. Reviewing the unverified WIP changed four things: it ended a log for
a received seal before verifying it; it kept a received seal that lost at
the coordinator out of trusted nodes' graphs while devices without a log
admitted it, so the two could follow different continuations; it configured
every opening at receipt, filling the 8 open logs with unused branches; and
it ended a tip's log on a bookmark deletion, which other bookmarks on that
tip may still follow.

### Contracts

- A trusted node verifies each new seal, then ends its tip's log with the
  seal's digest unless the log ended already, before the seal enters its
  graph. Any seal of an ended log is admitted, and the graph's lower digest
  decides between concurrent seals everywhere. Publication fails `sealed`
  only when a seal this node holds ended the log, and
  `coordinator_unavailable` when unconfirmed; an unconfirmed received seal
  waits outside the graph with its dependents.
- Moves have no entry of their own: a move off an open tip carries the
  close seal that ends its log; deleting a bookmark leaves its tip open.
- An opening's minimum holds for its whole line; trusted nodes relay it
  before the first join and never join a log with a smaller one.
- A write the terminal entry aborted is carried into the continuation of
  the tip's winning seal in the proposer's graph; a continuation whose seal
  lost hands it on.
- Joining a tip's log leaves the node's other one (`Backpressure` while it
  has a pending entry), so a node that switches bookmarks writes again.
- Members fence, record and rebuild entry evidence by the tip each frame
  names, for any log.
- Before a trusted node takes anything of a continuation's log, it decides
  and catches up the sealed tip's log through its terminal entry.
- A node holding committed entries of a dropped ended log refuses its slots
  `Sealed`.

### Completion-gate tests

| Gate | Tests |
| --- | --- |
| A write on `dev` commits through `dev`'s log; `main`'s set sees none | `tests::coordinated::seals::a_dev_write_commits_through_devs_log_while_mains_in_sync_set_sees_no_entry`: a trusted node in sync on `main` switches to `dev` under a line node, leaves `main`'s set, writes through `dev`'s log at the opening's minimum of one; `main`'s writes neither wait for nor include it |
| Committed before a seal is part of the continuation | the test below (0001 lies beyond the seal's frontier) and `a_trusted_node_joins_a_continuation_holding_what_committed_before_its_seal` (a fresh trusted node catches up the sealed log and numbers on) |
| A pending write retries there and commits once | `a_write_a_seal_aborted_retries_in_the_continuation_and_commits_once`: the seal is published through `publish_tip_statements`, the write commits as the continuation's entry 1 and appears once on both trusted nodes |
| Seals enter the graph only after their log ended | `tests::tips::a_coordinated_seal_enters_the_graph_only_after_ending_its_tips_log` |
| Joining leaves the previous tip's log | `rocky-coordinator machine::tip_tests::a_node_joining_another_tip_leaves_the_log_it_was_in_sync_on` |
| Dropped ended logs | `tests::coordinated::tips::a_node_holding_entries_of_a_dropped_ended_log_never_rejoins_it` |

### Verification

On 2026-10-15 every commit of the stack passed `cargo check --workspace
--all-targets`, and `./scripts/lint` and workspace Clippy passed.
`./scripts/check` ran twice: 167 unit tests (budget 10 s) and 949
integration tests in 140 s (budget 300 s) passed, but each run failed
`rocky-sdk::custody devices_never_online_together_converge_via_keyless_custody`
once, in a different suite, with a retryable `missing_dependencies` read.
The test passes alone; under 12 parallel copies it failed 7 of 36 times here
and 3 of 36 times at lane L's `sxumowmo`, before this integration. It
touches no coordinated path, so it is recorded as an existing load-dependent
flake, not fixed. The coordinated gate tests met their 2 s budget in
isolation (163 ms for the `dev` test). Kotlin, the shopping check and E2E
were not run; no Kotlin or native ABI source changed.

### Limits after the integration

- A carried write keeps its frames' tip and log position, the sealed tip's:
  members cannot rebuild its entry evidence, and a cut of the continuation
  could misjudge its fence. Re-signing it for the continuation is not done.
- Only a continuation's own sealed parent log is caught up, while retained;
  a branch tip's log checks nothing of its opening seal's log.
- A trusted node that held nothing of a dropped ended log could join it
  again until its seal arrives.
- An own write a crash left undecided on a tip the node left for another
  line stays unsent, with its effects.
- Moves are not log entries of their own, unlike the brief's wording; the
  close seal a move off an open tip requires is.
- Kotlin and the JSON API expose no seal call yet (slice 5, in progress
  separately).
