# Phase 10: stuck recovery evidence

Status: superseded by [phase 19](19-recovery-as-a-bookmark-move-evidence.md),
which removed the cuts recorded below and passes this phase's gate with the
owner's recovery seal. Collection remains. The rest of this record is history.

## Implemented slice

- Fresh trusted nodes learn the coordinator's genesis anchor after commits.
- Members serve entry candidates reconstructed from complete retained applied
  bundles, original frame IDs, signed positions and void evidence. The scan is
  paged at 256 operations and retains at most 16 response candidates.
- Every bundle member agrees on its transaction, openings and log position.
  Scope authorization applies to every returned frame. Staged frames are excluded.
- Catch-up requires every entry hash in the coordinator's current window,
  including when its final running hash matches. Missing copies and older gaps
  remain `Behind`; they cannot join or append.
- Existing SDK live-link catch-up already queries every connected device, so
  the new member evidence path needs no new SDK or Kotlin operation.
- Coordinator store/snapshot format 4 includes the genesis anchor and refuses
  formats 0, 1, 2, 3 and 5 in open/restore tests. Client evidence remains format
  3; encrypted replica v18 and link v3 stay unchanged.

## Scenarios

`rocky-secure` `tests::coordinated` adds:

- `a_fresh_trusted_node_rebuilds_from_a_member_after_the_in_sync_set_is_lost`:
  publish more than one catch-up page to a member, drop both trusted replicas,
  delete their database files, reconstruct at a fresh identity from the member,
  then commit the next position after the old set misses its deadline. No owner
  signature is supplied. The member observes the continued rows.
- `a_missing_copy_or_a_gap_outside_the_hash_window_never_rejoins`:
  no copy prevents a slot; a deterministic head whose window lacks the entry
  also prevents a slot despite an available copy. Projection and log position
  remain unchanged, and the node is absent from the in-sync set.

The existing integration filter covers these tests with the unchanged 2 s
per-test budget. The deadline clock is advanced only after a test signal observes the new
node's successful storage acknowledgment; it does not depend on an arbitrary
sleep or advance between assignment and acknowledgment.

## Verification

Passed:

- `./scripts/check --unit`: 157 tests.
- `cargo test -p rocky-secure --locked tests::coordinated -- --nocapture`:
  eight tests in the development run. The final fixture is covered again by
  the workspace integration check.
- `cargo test -p rocky-coordinator --locked coordinated`: eight machine tests
  and the persistent coordinated-log integration test.
- `cargo test -p rocky-coordinator --locked unsupported_store_versions_refuse_open_and_restore_before_writes`:
  one refusal test, 1.35 s.
- `./scripts/lint`: exit 0 on the latest source.
- `scripts/commitlint --jj @`: exit 0; the empty ancestor is not checked.

The first `./scripts/check` returned exit 1. Clippy and 174 unit tests passed;
integration had 1,022 passes and three timeouts. Its exact failure lines were:

```text
     TIMEOUT [  30.018s] (243/772) rocky-runtime retention_tests::a_failure_at_any_step_leaves_a_file_that_reopens_and_audits
     TIMEOUT [  30.017s] (105/253) rocky-sdk node::sync_tests::change_driven::representative_session_cancellations_are_atomic_and_repeatable
     TIMEOUT [  30.031s] (112/253) rocky-sdk::bootstrap a_cut_join_resumes_its_pages_and_keeps_edits_from_both_sides
test-tiers: failed: rocky-runtime retention_tests::a_failure_at_any_step_leaves_a_file_that_reopens_and_audits
test-tiers: failed: rocky-sdk node::sync_tests::change_driven::representative_session_cancellations_are_atomic_and_repeatable
test-tiers: failed: rocky-sdk::bootstrap a_cut_join_resumes_its_pages_and_keeps_edits_from_both_sides
test-tiers: over the integration budget: rocky-sdk::removal devices_that_reach_a_readded_field_by_different_steps_agree_on_it, 3.07s, budget 2.00s
test-tiers: over the integration budget: rocky-sdk::removal devices_that_reach_a_readded_field_by_different_steps_agree_on_it, 2.53s, budget 2.00s
```

All three timed-out tests passed when their existing test binaries were invoked
alone with `--exact <test-name> --nocapture`: retention 0.34 s, cancellation
1.37 s, cut join 1.96 s. Their sources are unchanged; this does not erase the
failed full check. The initial workspace load was 64.4 on 18 cores. Full logs
remain at `/tmp/rocky-phase10-check.log` and the isolated results at
`/tmp/rocky-phase10-retention-alone.log` and
`/tmp/rocky-phase10-sdk-timeouts-alone.log`.

The first four-worker retry also returned exit 1: 174 unit tests and 1,024
integration tests passed, with the existing SDK invoice gate failing. Its exact
diagnostic was:

```text
thread 'trusted_devices_number_concurrent_invoices_without_gaps' (69148025) panicked at crates/rocky-sdk/tests/coordinated.rs:95:5:
no invoice committed
```

The isolated workspace run repeated it (exit 100). The improved diagnostic then
reported `conflict` after 200 retries, with the domain still at commit 0, pending
position 1 and two members. Comparing the prerequisite implementation from
`uywtwqpr` passed in 0.56 s; temporary diagnostic/ablation edits were restored.
The initial-sync fixture was timing-sensitive: it waited for live links but did
not finish initial sync before concurrent writes. The fixture now awaits one
sync in each direction and preserves its last error/head on retry exhaustion.
No retry count or timeout was increased. The package gate then passed in 0.62 s.
Logs are in `/tmp/rocky-phase10-check-final.log`,
`/tmp/rocky-phase10-sdk-coordinated-alone.log`,
`/tmp/rocky-phase10-sdk-head-diagnostic.log`,
`/tmp/rocky-phase10-parent-sdk-coordinated.log`, and
`/tmp/rocky-phase10-sdk-synced.log`. One temporary ablation did not compile
because its parent bundle helper was private; that diagnostic is preserved in
`/tmp/rocky-phase10-sdk-without-membership-check.log` and was not a test run.

The final `NEXTEST_TEST_THREADS=4 ./scripts/check` passed (exit 0): Clippy,
174 unit tests (157 workspace plus 17 SDK without defaults), and 1,025
integration tests (772 workspace plus 253 SDK without defaults). Integration
wall time was 193.00 s against the 300 s budget. The recovery-from-member test
passed in 0.761 s and the missing-copy/window test in 0.172 s. The synchronized
SDK invoice gate passed in 1.604 s with defaults and 1.976 s without defaults.
Budgets and the selected tests were unchanged. Final log:
`/tmp/rocky-phase10-check-verified.log`.

The final lint run passed. Only Rust and documentation changed; Kotlin and the
shopping example checks were not run. Unrelated concurrent `docs/ideas` edits
were preserved.

The initial targeted build caught a syntax error while adding the head field.
It was corrected, and the next targeted run passed all eight tests. The exact
compiler diagnostic was:

```text
error: expected one of `!`, `.`, `::`, `;`, `?`, `{`, `}`, or an operator, found `:`
   --> crates/rocky-coordinator/src/coordinated.rs:410:20
    |
408 | impl Head {
    |           - while parsing this item list starting here
409 |     pub fn port(&self) -> rocky_core::LogHead {
410 |             genesis: self.genesis,
    |                    ^ expected one of 8 possible tokens
...
430 | }
    | - the item list ends here

warning: `rocky-coordinator` (lib) generated 1 warning
error: could not compile `rocky-coordinator` (lib) due to 1 previous error; 1 warning emitted
warning: build failed, waiting for other jobs to finish...
```

The warning above was `warning: ignoring -C extra-filename flag due to -o flag`.

## Cut slices

[Decision 0074](../decisions/0074-owner-signed-recovery-cuts.md) replaced the
earlier remaining contract. Its landing steps:

1. Coordinator admission of owner-signed cuts at `a+1` (`nvyxtvru`).
2. Checkpoint binding, device fencing and the five-step rollback (`tzssvuyt`).
3. `EncryptedReplica::cut_proposal`, `CutProposal::sign` and `submit_cut`
   (`rokvryvk`); see [stuck recovery](../runtime/stuck-recovery.md#proposing-and-submitting).

Step 3 adds three `rocky-secure` `tests::coordinated::cuts` scenarios:

- `a_proposed_cut_is_signed_submitted_once_and_never_reuses_a_position`: the
  proposal after collection equals the expected record; a non-owner signature
  is refused before any flush; an unavailable coordinator keeps the flushed
  record and resolution sends it again; a repeat answers position 3 and a
  stale record `Conflict`; the next entry is 4. A returning member reverts
  abandoned entry 2, and its frames relayed back have no effect.
- `a_proposer_whose_base_holds_an_abandoned_entry_fails_closed`: the proposer's
  base report marks its tainted checkpoint; after the cut the scope is guarded,
  its rows unchanged, and checkpointing fails with `cut_base_missing`. Another
  holder refuses that checkpoint as a base.
- `a_cut_whose_reply_is_lost_is_admitted_once_on_the_next_resolution`: the
  coordinator admits, the reply is lost, and resolution admits it locally once.

Step 3 verification, 2026-10-15:

- `cargo test -p rocky-secure`: exit 0; 284 passed, 11 ignored. The ten cut
  scenarios take 1.74 s together.
- `cargo test -p rocky-coordinator`: exit 0.
- `cargo test -p rocky-sdk`: exit 0. No SDK source changed.
- `./scripts/check --unit`: exit 0; 165 tests.
- `./scripts/lint`: exit 0 after `./scripts/lint --fix` applied rustfmt.
- `scripts/commitlint --jj @`: exit 0.
- `./scripts/check`: exit 0; Clippy, 182 unit and 1,045 integration tests
  (201.86 s against the 300 s budget). Log: `/tmp/rocky-p10s3-check.log`.

Remaining (step 4): the SDK native operation over live-link peers, the JSON
API, Kotlin, guides, and the Raft-store and process-kill tests 0074 lists.
The base report covers the proposer's own bases; peer bases are classified
only when offered for import.

## Limits

The collection scenario is in-process SQLite and a memory coordinator, with
physical deletion of the old database files. It does not qualify Raft recovery,
real-network loss or power loss; cuts are qualified in-process only. A serving member needs a complete bundle;
this slice adds no collector for fragments spread across partial devices. A
checkpoint-only copy does not reconstruct pruned entry evidence. No Kotlin,
shopping, Phase 5, Phase 7 or Phase 11 work is included.
