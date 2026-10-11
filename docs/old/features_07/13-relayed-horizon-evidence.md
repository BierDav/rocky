# Phase 13: relayed horizon evidence

Workspace: `p13-relayed-horizon`, based on main `a7d24a97`, 2026-10-10.
Implemented and verified. No push or main movement; edits remain in the
current jj change `nyqysuvs`.
[Decision 0055](../decisions/0055-relayed-horizon.md) defines the replaced
horizon, session and storage formats.

## Acceptance coverage

The `rocky-secure` integration family
`tests::partition_foreign_keys::horizon::relay` exercises real SQLite replicas
and authenticated in-memory sessions:

- The issuer and participant exchange native sessions only with custody;
  the relay retains their signed statement and acknowledgment across reopen.
  Both readers finalize and author afterward. Custody cannot acknowledge.
  Finalization before acknowledgment retrieval retains the original signed
  acknowledgments in a bounded completed certificate.
- Altering an acknowledgment, signing another statement ID or signing as a
  nonparticipant is refused. Finalization clears custody's pending evidence.
- Four combinations of sibling admission and acknowledgment delivery order
  converge on the lower ID. Two orders include the higher ID finalizing first.
  A separate different-cut case replaces the finalized higher replay base;
  the guarded holder imports the winner and preserves identity high-water marks.
- A stale nonparticipant receiving only a completed certificate enters the
  existing rebootstrap guard and imports the finalized checkpoint.
- A revocation arriving after declaration removes its participant from the
  required wait without invalidating the declaring policy reference.

The existing `network::holdings::tests` transcript sweep includes the new
relay request family: an unproved existing scope and an unknown scope receive
identical errors, zero evidence messages and EOF, against readers and custody.

SDK integration tests exercise a registry record whose device has never synced,
its recorded expiry, a missing record, default registry selection and the named
65-participant refusal. The existing shopping hub e2e gate now carries every
horizon through custody, with the participant never opening a session with the
declaring issuer, including setup. Author credentials are assigned through local
API calls. The post-move enrollment gate now joins after horizon finalization
and checks the imported finalized checkpoints.

The Kotlin shopping qualification prepares devices, issues credentials and
assigns authors through packaged APIs before any sync. Its participant never
opens a session with the issuer. It uses alternating custody rounds for all
nine group/table horizons. Its existing shopping CI job and the Rust e2e
job own these gates. No budget exception or budget increase is introduced.

## Verification

The first focused secure run passed revocation, the two initial sibling orders
and the higher-finalized recovery scenario. The relay fixture initially kept
its serving stream open and met the native read timeout; the fixture now closes
the client after the exchange. The first full check caught two collapsible
`if` blocks; both are corrected. Initial lint required installing the pinned
`ktlint` through `scripts/tools` in this workspace.

The custody gate also caught a stale nonparticipant receiving finalized evidence
before its parent rows had caught up. Such evidence now installs the existing
rebootstrap guard when the exact cut differs; the secure regression and the JVM
gate cover that recovery. The credential test now checks that an expired registry
deadline is immediately behind, while a zero expiry remains invalid.

- `./scripts/lint`: exit 0.
- `cargo test -p rocky-sdk --test shopping_release --locked`: all three e2e
  gates passed with no setup sync between participant and issuer (11.33 s),
  including custody relay and post-finalization enrollment.
- `ROCKY_SCENARIOS=gate ./examples/shoppinglist-app/qualify`: exit 0;
  JVM gate passed with independent enrollment in 35.747 s, including all nine
  custody-only horizons,
  offline C rebootstrap and empty `keys` tables on all three devices.
- `./scripts/check`: exit 0, including workspace Clippy and both the workspace
  and SDK-without-default-features test builds. Unit: 137 passed, 115.8 ms
  against 10 s. Integration: 808 passed, 129.16 s against 300 s, at load x1.0.
  All eleven new secure relay cases passed in 0.408–1.649 s against 2 s.
  The final budget audit reported no misses.
- `./scripts/commitlint --jj`: exit 0.

An earlier full audit passed all 808 integration tests and both suite budgets,
but its
isolated rechecks reported these misses (including one failed isolated rerun):

| Test | Best isolated, load-scaled time | Budget |
| --- | --- | --- |
| `rocky-coordinator::public tenants_register_isolated_domains_and_cannot_starve_each_other` | a failure | 5.00s |
| `rocky-sdk::registry_revocation an_unrecorded_delegate_uses_full_rotation_fallback_without_revoking_its_recorded_delegator` | 2.20s | 2.00s |
| `rocky-sdk::registry_revocation two_issuers_revoking_the_same_non_reader_leave_two_statements_and_one_tombstone` | 4.94s | 2.00s |
| `rocky-sdk::shared_lists a_list_received_from_a_checkpoint_keeps_its_rows_after_reopening` | 2.99s | 2.00s |
| `rocky-secure tests::chains::a_chain_goes_with_the_last_operation_that_names_it` | 7.27s | 2.00s |
| `rocky-secure tests::chains::a_command_table_bootstraps_by_checkpoint_under_20_and_40_grant_chains` | 6.58s | 2.00s |
| `rocky-secure tests::chains::a_session_carries_an_authors_chain_once_across_its_pages` | 10.47s | 2.00s |
| `rocky-secure tests::checkpoint::pruning_needs_a_verified_checkpoint_acknowledgments_and_custody` | 7.15s | 2.00s |
| `rocky-secure tests::constraints::unique_values_converge_in_any_order_and_the_loser_returns` | 7.70s | 2.00s |
| `rocky-secure tests::checkpoint::checkpoint_binds_the_scope_and_survives_a_failure_at_every_step` | 8.91s | 2.00s |

No test-tier exception was added. The final run passed every isolated
recheck within its existing budget.
The Rust shopping rerun also found a background-worker stack overflow in the
larger async admission path; boxing its admission futures fixed it, and all
three Rust e2e gates pass with the default worker stack.
