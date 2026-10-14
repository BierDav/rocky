# Phase 9: coordinated writes evidence

Status: slices 1–5 implemented; every completion gate passed (2026-10-14).
Workspace: default jj workspace, change on top of `xkykmxru`, macOS arm64.

## Prerequisites

Verified before editing: Phase 4's never-offered marks, whole-bundle voids
(`rocky.void.v1`), void admission and range/push transport, and rollback by
replay; Phase 8's `coordinated::Log`, signed acknowledgments, deadline
resolution, retry identities and `NetworkCoordinator::coordinated`. No
prerequisite needed repair. Phase 4's hold step was factored into a closure
voider that coordinated aborts reuse.

## Design choices

- A metadata-only `rocky_core::DomainLog` port (head, join, append,
  acknowledge) and `coordinated_chain`, shared with the coordinator. The
  coordinator implements it in `NetworkCoordinator`; `coordinated::MemoryLog`
  runs the same log machine in one process, with deadlines resolved on the
  next call as the leader timer does.
- The entry hash commits to log, position and sorted frame ids. Frames carry
  the position in their bundle membership; the hash cannot be inside the
  frames it covers. A reader holding every member recomputes it.
- The proposer stages in the SQL commit transaction (flushed, append and
  sequence recorded) and keeps frames unsent: range pages stop at them,
  transfer pages skip them, `offer` refuses them, checkpoints of their scopes
  and recovery export wait. Committed own frames count as offered.
- A failed append voids at once: the entry was never sent nor acknowledged,
  so it cannot commit. Only `Undecided` keeps local effects, unsent.
- Peers store entries (frames and the voids they continue after) unapplied
  and acknowledge themselves; a store the coordinator refuses is deleted.
  Committed entries apply in log order through ordinary receive.
- Catch-up carries positions, hashes and frame ids verified against the
  head's window and running hash; frames come by range sync. Committed entry
  records are kept for 1,024 positions.
- One async lock per replica serializes slot, propose and resolve; an
  in-process marker keeps resolution from driving a write `propose` is about
  to drive. Found by the SDK test, which raced the background resolver.
- SDK composition is a runtime `Database::coordinate(log)`; a coordination
  service database composes its service log. Entries travel only over open
  live links. JSON: `"coordinated": true` on `BEGIN`/`COMMIT`, a
  `coordinated` status call, typed codes. Kotlin: `RockySession.coordinated`
  with `CoordinatedSql.query`, `coordinatedStatus()`.
- Formats: `rocky.encrypted-replica.v18`, `rocky.link.v3` (new entry family).
  No migration or legacy reader.

## Gates

| Gate | Evidence |
| --- | --- |
| Concurrent invoices sequential without gaps on every device | `rocky-secure` `tests::coordinated::concurrent_invoices_are_sequential_on_every_device_and_members_see_no_aborted_entry` (a forced race, then 8 more invoices from both nodes; alice, bob and member carol hold 0001–0009); SDK `tests/coordinated.rs` `trusted_devices_number_concurrent_invoices_without_gaps` (two databases over live links, positions 1–6, keys 0001–0006 on both) |
| Crashes never leave an entry true on one node and missing on another | `a_proposer_crashing_before_its_append_commits_it_after_restart`; `crashes_before_acknowledging_never_leave_an_entry_true_on_one_node_only` (proposer after storing, node before acknowledging; minimum 1 commits on both, minimum 2 aborts on both) |
| A member never receives an aborted entry | The concurrent test: the loser's staged frames are absent from range and transfer pages while staged, and from the member after sync |
| A rejoining node proposes only after catching up | `a_node_that_missed_entries_proposes_only_after_catching_up`: evicted at a deadline, `Behind` while no peer serves entries, slot at 3 after catch-up, then invoice 4 on both |

Also: `without_a_coordinator_or_below_the_minimum_a_write_fails_typed_and_rolls_back`,
`entries_travel_over_live_links_and_catch_up_reads_their_positions` (a
three-frame entry over a real link; every member carries position 1) and
Kotlin `CoordinatedTest` (request shape, typed refusal, rollback).

## Verification

- `cargo test -p rocky-secure --lib tests::coordinated`: 6 passed, 0.69 s.
- `cargo nextest run -p rocky-secure --no-fail-fast`: 267 passed (before
  the link family); the full check below covers the final code.
- `./scripts/lint`: passed (after `--fix` formatting).
- `./scripts/check`: workspace Clippy, 174 unit tests (85.4 ms) and 1,023
  integration tests (183.99 s of 300 s), with and without SDK default
  features. Contended slow tests passed alone: the SDK gate 512 ms, the
  crash gate 553 ms, against 2 s.
- `sdks/kotlin/gradlew -p sdks/kotlin publishToMavenLocal` and `check`, both
  with `-Dmaven.repo.local=/tmp/rocky-phase9-maven`: passed (check 2 min).
- `./examples/shoppinglist-app/check` with
  `KOTLIN_CLI_JAVA_OPTIONS=-Dmaven.repo.local=/tmp/rocky-phase9-maven`: 59
  tests passed, desktop and Android builds succeeded. `~/.m2` was not written.
- `scripts/commitlint --jj @`: passed. The default range refuses the
  untouched empty ancestor `zllmttww`, which this task may not rewrite.

Initial `./scripts/check` failed workspace Clippy:
`too_many_arguments` on `author_bundle_carried` (8/7) and
`await_holding_lock` in `resolve`. Fixed by passing the bundle identity as
one pair and dropping the guard before the await; no lint was suppressed.

## Invoice liveness and void-counter correction (2026-10-15)

The Phase 5 full check exposed the live-link invoice gate failing intermittently
with `no invoice committed`; a direct no-default-feature run also failed with
`missing_dependencies` / `SQL session: items is not available: Incomplete`.
The assertions were valid. Investigation found two production bugs:

- A proposer sent storage requests once, then only polled the coordinator while
  holding its resolution lock. A transient missing/replaced live link left the
  entry pending although the peer became reachable. The proposer now retries
  storage with the existing bounded polling backoff, only while that exact
  position/hash remains pending. The original deadline, required acknowledgments
  and durability boundaries are unchanged.
- Importing a peer checkpoint reset the applied log frontier and could erase a
  local void's higher consumed author position. A later write reused that
  sequence; the receiver correctly refused it, and subsequent frames waited on
  its missing backlink. Authoring now compares the applied tip with the last
  voided position and continues after the newer one, including its hash and
  clock. The lookup uses the existing `(scope,author,sequence)` primary key.

`a_proposal_retries_storage_after_a_transient_link_failure` deterministically
refuses the first delivery, then requires the same proposal to commit before its
deadline and appear on both nodes. `importing_a_checkpoint_never_reuses_a_voided_author_position`
forces a lost race, imports the winner's checkpoint that lacks the loser's void,
and checks the next sequence/backlink, both projections and both audits.
The SDK invoice gate retains its original retries, six invoices and assertions.
No persisted or wire format changed.

The correction is isolated in change `ptxlnlqx`, directly on Phase 9
`uywtwqpr`. Phase 5 rollout code is a separate descendant. The SDK invoice
test and its retry counts, assertions and deadlines are unchanged.

Repair-only verification in `rocky-02` on 2026-10-15:

- `cargo build -p rocky-secure -p rocky-sdk --no-default-features --locked`
  passed.
- `cargo test -p rocky-sdk --no-default-features --test coordinated
  trusted_devices_number_concurrent_invoices_without_gaps --locked -- --exact
  --nocapture` passed **21 of 21** consecutive runs, with zero failures and
  no sleeps between invocations. Test durations were 0.47–0.57 s.
- `cargo test -p rocky-secure tests::coordinated --locked` passed all eight
  coordinated regression tests; output: `/tmp/rocky02-repair-regressions.log`.
- Full output: `/tmp/rocky02-repair-build.log` and
  `/tmp/rocky02-repair-repeat.log`. This sample establishes neither
  real-network reliability nor an absence of other concurrency failures.

## Limits

In-process coordinator (`MemoryLog`) and in-memory transports; the voter
service's `DomainLog` adapter is compiled but not exercised end to end here.
Kotlin has no composed `DomainLog` and no device-level coordinated test;
`CoordinatedTest` uses a fake transport. Not measured: per-write latency or
message count over links. Proposer reads (and remote reads) see staged
effects until decided. Decisions beyond the 1,024-hash window, stuck sets and
removing commands are phases 10 and 11.
