# Phase 8 coordinated-log evidence

Status: complete; all phase gates and required workspace checks passed.
Workspace: `rocky-02`, macOS arm64, 2026-10-14.

## Prerequisites and scope

Verified the existing certified controls, signed request/response lanes, tenant
accounting, linearizable reads, Raft apply/snapshot boundaries and durable store
before editing. The brief has no dependency on the schema phases. Root rules
still refer to the removed features_07 entry point; the explicitly assigned
features_09 contract supplies this task's roadmap authority.

[Decision 0072](../decisions/0072-coordinated-log-and-writes.md) settles the metadata
log and phase 9's store/commit/publication boundary. Phase 8 implements no payload
storage, bundle application, member publication, SDK/Kotlin write API or exceptional
owner recovery. These belong to later briefs, not substitutes in this log.

## Gates and meaningful regressions

- `machine::coordinated_tests::competing_appends_retries_and_exact_signed_acknowledgments`:
  one assignment at a competing head; exact retry/conflict/retirement identities;
  no early commit and no acceptance of an acknowledgment for another hash.
- `deadlines_evict_missing_nodes_and_minimum_prevents_success`: deadline eviction,
  commit with the remaining minimum, abort below two, and refusal to append with
  only one member. A behind node cannot rejoin on its old position/hash.
- `serialized_leader_recovery_keeps_pending_but_requires_new_acknowledgments`:
  metadata retains pending work and does not persist partial acknowledgments;
  duplicate resolution cannot advance a committed position twice.
- `tenant_rate_deadline_and_byte_budgets_fail_closed`: capped deadlines, rate
  admission and retry behavior, and capacity reserved before admitting a log.
- `deadline_resolution_uses_authority_at_the_signed_storage_acknowledgment`:
  a control expiring before the write deadline does not strand stored work.
- `log_roles_and_signed_proofs_cannot_bypass_domain_control`: members cannot use
  trusted-node APIs; duplicate or forged proofs and trusted-key overflows fail,
  and command closure cannot abandon a pending log.
- `hash_window_is_bounded_and_encoded_domain_footprint_is_measured`: 1,025
  committed positions leave exactly the newest 1,024 hashes, round-trip the full
  byte-string window, and stay below 40 KiB.
- `raft_store::tests::coordinated_pending_and_resolution_recover_from_logs_and_snapshot`:
  real synced log replay retains a pending assignment and retry; a snapshot
  retains its resolved position and rejects another commit of the same entry.
- `coordinated_log_survives_leader_change_and_deadline_without_double_commit`:
  three real voter processes race appends, stop the leader after one
  acknowledgment, require that acknowledgment again after election, commit once,
  resolve a missed deadline without an application resolving call, then snapshot
  and restart every voter without losing committed state.

Machine and storage scenarios are integration tests under existing 2 s budgets.
The multi-process election/deadline/restart gate takes 3.62 s alone and is explicitly
in e2e; it uses the production timer rather than a test sleep. No budget is raised.

## Measurements

The full-window measurement reported 1,524 CBOR bytes for a two-node domain plus
its control, and 33,485 bytes for its log after 1,025 commits. Random commitments
can vary the CBOR size by a few bytes. The hashes themselves are one 32,768-byte
string. Admission accounting reserves 60,928 bytes per active log, covering all
32 node/retry slots and the window; it is not an RSS measurement.

The live successful write measured three client calls (append and two storage
acknowledgments) and exactly two new durable Raft log entries (assignment and
resolution). Preparing the write adds a head read: four logical client calls
including preparation, three excluding it. Membership joins occur on connection, not
on each write. Transport retries and linearizable read barriers add voter RPCs;
this measurement does not count TCP packets, Raft heartbeat traffic or phase 9's
peer-to-peer payload messages.

## Verification

Passed verification:

- `./scripts/check --unit`: development unit run passed 155 workspace tests;
  the final full check below reverified them.
- `cargo test -p rocky-coordinator --locked`: 56 tests passed, including the
  existing fsync probe and all coordinator multi-process scenarios; one unrelated
  ten-window measurement remains ignored.
- `./scripts/lint`: all modules passed; the workspace's pinned ktlint was installed
  with `./scripts/tools ktlint` before the final lint run.
- `./scripts/check`: strict workspace Clippy, 172 unit tests and 975 integration
  tests passed, including the SDK without default features. Unit tier: 139.4 ms;
  integration tier: 162.46 s, within their 10 s / 300 s budgets. Contended slow
  tests passed the existing isolated budget rechecks; no budget was changed.
- `cargo test -p rocky-coordinator --locked --lib machine::coordinated_tests::hash_window_is_bounded_and_encoded_domain_footprint_is_measured -- --exact --nocapture`:
  final footprint capture above passed.
- `./scripts/commitlint --jj`: the current change's Conventional Commit description
  and requested co-author footer passed.

The published provider-signed genesis keeps its current shape. New log caps are
fixed in the machine alongside existing Usage budgets. The final isolated package
run passed the fsync probe's existing mean bound after a contended run missed it.

Not run: full workspace e2e, the unrelated ignored ten-window probe, Kotlin/Room,
shopping application, Android or iOS checks (no consumer/platform code changed).
No live public-service deployment, application bundle durability, RSS measurement
or TCP packet census is claimed. Later phases own bundle integration, recovery
cuts and removal of strong commands; no phase 8 slice or gate remains.
