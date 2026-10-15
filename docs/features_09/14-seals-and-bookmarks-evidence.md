# Phase 14 evidence

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

Lane L must replace its temporary `followed_tip()` with this implementation,
key logs by `(Scope, TipId)`, and integrate terminal seal/move entries. Both local
publication and received seals/moves explicitly refuse a configured coordinated
log until that integration; graph exchange cannot bypass the refusal. SDK/JSON/
Kotlin seal/follow/status/inspection surfaces remain wave 2.

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

## Lane S wave 2 — developer calls and guides

### Slice 5: SDK, JSON and Kotlin

`rocky-secure` adds `seal_frontier`, `tip_observation`, `tip_nodes` and
`line_node_needed`. The seal frontier takes the global author marks of every
locally held operation or void visible in the followed tip, at least its parent
seal's, and refuses pruned logs, whose global positions are gone. Status and
inspection decode operation headers, never rows. Peer frontiers come from
`secure_peer_frontiers` and peer tips from open links; neither invents a value
the protocol does not report.

The JSON request `{"type":"tips","action":...}` provides `status`, `inspect`,
`seal` (`close` defaults to false), `open` (`seal`, `minimum_in_sync`),
`create` (`name`, optional `tip`), `move`, `delete`, `follow` and `drop`.
`open` returns `{tip}`, `inspect` the graph and every other action the status.
`rocky_sdk::Session` has the same typed calls (`tip_status`, `inspect_tips`,
`seal`, `open_tip`, `create_bookmark`, `move_bookmark`, `delete_bookmark`,
`follow`, `drop_tip`) over `rocky_sdk::tips` records. Kotlin `Tips`, reachable
from `RockyClient.tips` and `RockySession.tips`, decodes them to typed values
with unsigned counters. The records are not UniFFI records; both Kotlin
clients use the JSON call.

The owner signs with no links, an identity holding a trusted grant for the
domain through that grant, and an issuer through its issuer links; others are
refused before signing, and admission verifies the authority again. A
`continue` seal and its bookmark move publish as one batch through
`publish_tip_statements`; a backward or sideways move off an open tip adds that
tip's `close` seal to the same batch. Only a device following a tip seals it,
so the frontier is its own. Each batch is checked against the graph it extends
first: a call the graph would not resolve to, such as a move losing to a lower
digest, is refused instead of published.

Rocky creates no bookmark. The owner creates `main` on the genesis tip and
follows it; other devices follow it once the graph reaches them. Until a device
follows a bookmark, status reports a null bookmark and sealing is refused.
Existing graphs keep refusing explicit generation upgrades, as before.

Each line other than the original node's gets its own node. Its key and
certificate are stored before registration in the secret-store role
`tip-nodes` (format 1; logout forgets it). A stored identity takes the next
free number of its minting ledger, saved before signing; a supplied identity's
provider reserves one of its node numbers. Certification uses the SDK's own
node certification, so issuer identities' line nodes carry their grants.
Nodes are registered after `follow`, after publishing, at open and after each
reconciliation pass; background failures are recorded as
`line_node_failed` activity and writes stay refused until a node exists.

Not provided, deliberately: `evidence_releasable` is null until
[phase 15](15-lazy-compaction.md) can compute it; remote kept tips and seals
are null because peers do not report them; the local node keeps no seal state
before [phase 16](16-kept-seals-and-views.md), so its `kept_seals` is empty.
`minimum_in_sync` is required because a device cannot know the coordinator's
domain minimum. The SDK has no call that grants a trusted identity, so through
the SDK only the owner opens tips and creates, deletes or moves bookmarks off
their line. A link attempt made while two peers follow different tips fails
and leaves a retry backoff, so the next explicit sync after following may
report `skip: backoff`; once on the same tip, rows sealed earlier arrive.

Targeted verification: `cargo test -p rocky-sdk --test tips` passed 7 tests:
continue seals keep every row and move `main`; `close` refuses writes;
a writer's seal and bookmark calls are refused; an issuer seals and moves
`main` forward but neither opens a tip nor moves `main` back; malformed
requests fail; a
branch writes under its own node, keeps it across reopen and returns to `main`,
and dropping it with unsent writes is refused; moving `main` back closes its
tip, holds the abandoned write and accepts new writes on a fresh line node;
a device on `main` receives none of `dev`'s frames while the graph lists both
bookmarks; the typed `Session` calls match the JSON contract.
`sdks/kotlin/gradlew -p sdks/kotlin :rocky-core:jvmTest` passed, including 7
`TipsTest` cases for request shapes, unsigned values beyond 2^53 and errors.

### Slice 6: guides

[Seals, tips and bookmarks](../runtime/tips-and-bookmarks.md) now starts with
the developer tasks: the first bookmark, sealing, branches and line nodes,
bookmark moves, and the status and inspection calls in Kotlin, Rust and JSON,
followed by the replica boundary and late and held writes. The docs index lists
it before generations; the checkpoint entry links the
[horizon](../runtime/mergeable-horizon.md) as the release mechanism until
phase 15, and the horizon and generation guides say what replaces them. SDK
enrollment lists the `tips` native call and the `tip-nodes` secret.

### Final wave 2 verification

`./scripts/lint` passed. `./scripts/check` passed Clippy, 167 unit checks and
952 integration checks: 704 workspace tests plus 248 SDK tests without default
features, in 143.20 seconds against the 300-second budget. The three tips tests
slower than 2 seconds under load confirmed alone at 0.22, 0.39 and 0.50
seconds; no budget exception was added. `sdks/kotlin/gradlew -p sdks/kotlin
check` passed, running `rocky-core`'s JVM and iOS simulator tests, and
`./examples/shoppinglist-app/check` passed.

A plain `cargo test -p rocky-sdk`, run alongside other builds, failed the
e2e-tier `a_migration_of_100000_rows_converges_on_every_member` (no convergence
within 64 syncs). It uses an explicit generation upgrade without a tip graph,
which the new calls do not reach, but it was neither rerun alone nor compared
with the parent. E2E, Android device and packaged iOS qualification were not
run. Coordinated seals and moves, trusted-identity grants through the SDK and
the completion gate's coordinated-write items remain open.
