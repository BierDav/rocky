# 2. Change-driven sync

Status: complete; all local phase-2 gates pass on main `a53168f0`, including phase 6 (2026-10-10).
Owner: Kotlin `rocky-core` automatic sync and lifecycle (`RockyDatabase.kt`,
`RockySession.kt`); the `rocky-sdk` sync worker in `node.rs` and
`scheduling.rs`; the always-on peer, which runs the same worker. No
dependency. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the entry on Kotlin automatic sync in
[fix later](../fix_later/README.md),
[change-driven sync](../runtime/always-on-peer.md#change-driven-sync),
[sync](../runtime/sdk-enrollment.md#sync), the
[transport bounds](../runtime/sdk-transports.md#bounds),
[sync wakeups](../runtime/sdk-transports.md#sync-wakeups) and
[Android lifecycle](../runtime/android.md#lifecycle).

Kotlin's default `automaticSync` calls `syncOnce` every 500 ms, and each pass
dials every peer in turn. `enterBackground` waits for the running pass, which
Kotlin runs as non-cancellable, so an unreachable peer delays the pause by up
to 15 s per route; with a stopped peer one desktop sync took about 75 s. The
native worker behind `syncContinuously()` has no timer while idle, but it
keeps one retry schedule for the whole pass, every local write resets it, and
disabling it does not interrupt a pass. A mesh multiplies the peers, and iOS
suspends a process that does not pause promptly.

## Settled

- A pass reconciles peers concurrently, at most four at a time. A sequential
  pass cannot finish while one dial is stalled.
- Back-off is per peer, not per route and not per pass: 1, 2, 4, 8, 16, 32
  and then 60 seconds, as the worker's schedule is today.
- A local write does not reset a peer's back-off. A connectivity signal,
  `setOnline(true)`, a change of that peer's configuration or an inbound
  session from that peer does.
- `syncOnce` and the native `sync` request ignore back-off, dial every peer
  and keep their response shape.
- The pause is `sync_start` with `enabled: false`. It cancels the running
  pass and returns within 1 s. No UniFFI export is added.
- The always-on peer gets the same behavior; its guide is updated with it.

## Ordered slices

1. Worker: per-peer back-off state replaces the single retry of the pass.
   The worker skips a peer whose back-off has not elapsed and sleeps until
   the earliest deadline; a pass with nothing to send and every peer backed
   off makes no dial.
2. Worker: peers reconcile concurrently within the bound. The pass reports
   its status once every dialed peer has answered or failed.
3. Cancellation: disabling the worker drops the running pass at its next
   await. The replica keeps what was committed; a frame is stored whole or
   not at all, and the next pass repeats the rest.
4. Kotlin: `automaticSync` enables the native worker and observes its status
   as `syncContinuously()` does; the polling loop is removed.
   `enterBackground` disables the worker and returns within the bound;
   `enterForeground` enables it, which starts a pass at once. `syncOnce`
   stays. These edits land before
   [phase 17](17-kotlin-native-binding.md) moves the native sources.
5. Remove the automatic-sync entry from fix later. Update the four guides
   named above: the 500 ms sentence, the retry schedule, the pause and the
   statement that a sync makes one attempt per route.

## Completion gate

All Rust items run on `rocky_sdk::transport::memory::Hub` under Tokio's
paused clock.

- With one of three peers `stalled`, the first pass reaches the other two and
  reports them before the stalled dial times out. Later passes do not dial
  the stalled peer until its back-off elapses, counted by the hub's dials.
- A local write during the back-off dials the two reachable peers only.
  `network_changed()` and an inbound session from the stalled peer each
  cause one dial of it at once.
- `sync` dials all three peers whatever their back-off.
- With a dial stalled, `sync_start` with `enabled: false` returns within 1 s
  and the hub counts no later dial. In Kotlin `enterBackground` returns
  within 1 s and `enterForeground` produces a pass without waiting for a
  timer.
- A pass cancelled at every `cut_after` byte offset of a session that pushes
  and pulls loses no local row and stores no partial frame; the following
  pass converges.
- An idle database whose peers are all unreachable makes no dial for one
  simulated hour without a signal.
- The always-on peer's configuration test and `./kotlin check` pass.

## Retry work interpretation

A failed peer retains its deadline even when no work remains. Only pending
frames, a requested metadata refresh (such as installing a key) or continuation
arm a retry timer; an idle unreachable peer waits for a signal. This meets
both the per-peer schedule and the one-hour idle gate. Local writes make work
pending without resetting that deadline.

## Measured evidence (2026-10-10)

- `NEXTEST_PROFILE=default NEXTEST_TEST_THREADS=4 ./scripts/check` passes:
  718 workspace tests (16 baseline skips) and 169 SDK tests without default
  features (5 skips). `cargo test --workspace --doc --locked` also passes.
  The baseline includes
  nine paused-clock change-driven tests and the always-on configuration test
  `configuration_enrolls_resumes_and_refuses_reading_identities`.
- The hub records both healthy peers before the stalled peer's 15-second
  deadline, preserves its deadline on a local write, and retries immediately
  on connectivity, peer configuration and authenticated inbound signals.
  Manual sync attempts all peers. Five configured peers start only four
  stalled attempts. Both successful automatic workers settle without polling;
  an unreachable idle peer makes no further dial over a simulated hour.
- Native disable cancels a stalled pass without advancing the paused clock.
  Kotlin's `SyncLifecycleTest` passes against a deliberately suspended custom
  dial: background returns in **5 ms**, and foreground makes its first dial
  in **4 ms**, each under a 1-second deadline. No sleeps drive that test.
- The exhaustive release-profile gate **passes all 46,251 offsets**, including
  both boundaries, in **2,010.38 seconds** (33 min 30.38 s),
  after the final checkpoint and Kotlin repairs. Each trial audits both replicas
  before healing
  and checks local rows and final convergence. SQLite fixtures use a temporary
  RAM disk with flushes enabled; this qualifies cancellation, not power-loss
  durability.
- `./scripts/lint`, `scripts/commitlint --jj`, `./sdks/native/check` and the
  Android runtime's `publishToMavenLocal` pass. Phase 2 itself adds no UniFFI
  export and changes no persisted or native-session format. CI's shopping job
  runs full Kotlin checks and the exhaustive release-profile gate; no hosted
  run was performed.

### Final integrated run

On main `a53168f0` with the phase-4 household fixes retained, `./kotlin check`
passes: all 57 Kotlin tests, including 14 Room qualification tests and 28
shopping tests, plus the packaged consumer and qualification tasks. The
consumer keeps exact allocation assertions: the registry uses author 1, the
executor reserves 2–3, the issuer assigns 4, and the executor assigns 3 after
delegating 2. These match phase 5's current allocator.

`./sdks/native/check` and Android `publishToMavenLocal` pass on this state.
The first Kotlin run passed its tests but refused a stale Android runtime digest;
rebuilding both packaged ABIs and rerunning the full check resolved that failure.

The phase-6 rebase exposed concurrent renewal failures in both partitioned-issuer
revocation acceptance tests. The pass now finishes its initial concurrent
sessions, renews once and rechecks successful old handshakes and credential
failures under the refreshed authority; unavailable routes keep their back-off.
Both unchanged acceptance tests pass in both full Rust configurations and in
10 stress iterations each. Phase-6 revocation sets and bootstrap intents are
retained, with no epoch compatibility reader. The subsequent rebase onto
`19bb76f8` changes only CI retries; `a53168f0` changes CI artifacts and makes
doc tests opt-in. The merged shopping job retains x64 Android qualification
and downloads the separate GitHub peer artifact. Cancellation inputs are
unchanged. Local checks use the default nextest profile; doc tests also run
explicitly.

The shopping CI job uses x64 for the Android NDK. It reuses the custody peer
artifact on GitHub and builds it on Gitea, whose baseline artifacts are arm64.
Actionlint and ShellCheck pass; hosted execution remains unverified.

The idle and metadata-retry fixtures now configure an unreachable contact on
an empty database, with every frontier asserted empty. Enrollment creates
registry work under phase 5 and was not an idle fixture. Both retain their
retry/no-dial assertions and pass 100 stress iterations each.

Earlier default-parallel Rust runs failed the fsync mean bound under concurrent
qualification load, then a voter's TCP bind on restart. The fsync probe and
coordinator application test pass in the full four-thread run; no assertion or
timeout was relaxed. Kotlin's probe-then-bind TCP port race is removed by
letting the native listener allocate port 0 in Room and shopping fixtures;
the delivery assertions are unchanged. Lint's RAM-volume permission failure
disappeared after unmounting the temporary fixture volume.

### Kotlin integration repairs

The repeated approvals timeout also exposed historical checkpoint verification
refusing a member's explicit table grants because its credentials carried
unrelated list partition selectors. The checkpoint verifier now checks those
grants while retaining the node-kind, signature, generation and whole-scope
read/write requirements. Concrete partition checkpoints still verify their
tuples. `results_sync_with_unrelated_partition_selectors` fails before the
repair and passes afterwards. The unchanged Kotlin approvals test also passes
in the full check and five additional repetitions. See [strong history](../runtime/strong-history.md).

A full Kotlin run exposed a read lease timeout that ended a shopping background
collector and failed the approval regression. `settledOnRocky()` now retries
that specific rolled-back read through its existing bounded back-off. A
virtual-clock regression checks recovery of committed rows and propagation
of permanent storage errors. No transaction budget or acceptance timeout is
increased. See [Room observations](../runtime/room-integration.md).

The Room trace fixture's `installAuthors` extension was shadowed by the SDK's
member method. Renaming it to `installRoomAuthors` restores the fixture's
explicit lists/items author order; DAO and recorded transaction assertions
remain in place.

Every joining profile now announces its authenticated return route. The
issuer saves it as a configured peer and wakes sync after enrollment commits;
only an issuer profile joins the issuer set. A paused-clock regression joins
a reader, settles its initial handshake, writes later on the issuer and
observes delivery without polling or advancing time. The existing 16-peer
bound remains; broader contact exchange belongs to phase 14.

The shopping household remembers each invitation choice by the native
invitation event's sequence, under a mutex, and closes only that invitation's
code. A deterministic regression delays the collector until two enrollments
and a newer invitation exist, then verifies both device names/kinds and that
the newer code remains open. The original key-delivery and revocation/rejoin
tests retain their assertions. See [invitation lifetime](../runtime/shopping-devices.md).

Rebasing onto the phase 5 issuer registry also required current-format
qualification expectations: phase 6 supports identity version 11, the registry
consumes an author number, a member has no issuer-registry scope, and removing
the last issuer is allowed. Version refusal still checks both files unchanged;
author identity/ownership and application horizon digests remain checked.
Member-to-member sync is also checked after removing the issuer.

Not verified: hosted CI, physical-device lifecycle timing, NAT traversal or
power-loss qualification. Local fault injection uses the paused clock and
byte cuts; SQLite flushes remain enabled.

## Not in this phase

- Ranking peers, a cap on peers per pass or learned contacts
  ([phase 14](14-contacts-and-partner-choice.md)).
- Releasing the replica between apply transactions and the Room write
  timeout ([phase 3](03-catch-up-cost.md)).
- Background sync, push wakeups or socket rebinding on iOS
  ([phase 18](18-ios-packaging-keychain-lifecycle.md)).
- A coordinator watch protocol, a new UniFFI export or a changed `sync`
  response.
- Changes to `RockyLifecycle` on Android beyond what the pause requires.
- Reads on the writer thread
  ([known costs](../runtime/scaling-limits.md#known-costs)).
