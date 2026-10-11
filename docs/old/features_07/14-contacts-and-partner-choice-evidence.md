# Phase 14 qualification

Implemented on 2026-10-10 in the `p14-contacts-and-partner-choice` workspace,
initially based on main `ff86c729` (phase 10), then rebased without conflicts
onto `4f43c453` when phase 18 advanced main during verification. Phases 2 and 9 supply the native
worker/back-off and roster-free admission. The earlier partial change was kept.
[Decision 0056](../decisions/0056-contact-exchange-and-discovery-secrets.md)
records native session 11, identity 13 and discovery secret-store record 1.
Enrollment, renewal, invitations and contact URI formats are unchanged.

## Acceptance coverage

The eight SDK tests in `mesh::tests` and `node::sync_tests::mesh` use real
SQLite and secret providers, with admitted sessions over the in-process hub:

- Starting with one contact, a member learns a second by handover and a third
  by node-ID request, removes the issuer and syncs with both learned peers.
- A sufficient local contact is the sole dial. `peers()` reports
  `selected_local` for it and `covered` for the skipped issuer.
- A reader with a concrete partition key tries a local peer lacking that
  partition, reaches its keyless custodian within two passes and reads the
  custodian's row after the issuer is detached. The custody reply covers the
  concrete needed scope; the insufficient peer reports refusal.
- Admission refuses a revoked device before contact/discovery exchange, and
  its secret store still lacks discovery material. An admitted member gets
  the root and value, but neither a handed-over contact nor a requested
  contact for the revoked node. Revoked and unknown requests both return null.
- Equal live sets and values derive equal topics. A missing statement or value
  differs, a modified signed value is refused, and the previous topic expires
  after 24 hours. The topic read verifies stored signed values.
- At capacity the oldest eligible learned contact is evicted; application
  routes and the enrollment forwarding upstream survive replacement and aging.
- Every replica table is scanned for the learned route and discovery root and
  value bytes, hexadecimal encodings and JSON value encodings. None occurs.
  Neither discovery fields nor a contact table occurs in the replica schema;
  `identity.json` contains no discovery material.
- A failed peer is not dialed again while its back-off lasts, including an
  explicit pass, and `peers()` reports `backoff`. An explicit pass with only
  backed-off contacts returns `unreachable`, preserving bootstrap failure
  reporting. Connectivity signals reset the deadline.

The request test accepts eight requests, rejects the ninth locally and rejects
an answer whose key hashes to a different node ID. The additional
`rocky-secure` `network::contacts::tests` test bypasses the client's guard and
proves the server rejects request nine before calling its contact handler.

## Tiers and measurements

All these scenarios belong to integration: they use files, runtimes or
sessions. `.config/test-tiers.toml` explicitly includes the SDK `mesh` module
and secure `network::contacts::tests`; the existing SDK `node` filter includes
the session acceptance tests. `scripts/test-tiers` enforces the ordinary 2 s
alone and 5 min suite budgets, also in CI through `scripts/check`. No exception
or larger budget is added. See [test budgets](../development.md#test-budgets).

Host: Apple M5 Pro, 18 cores, macOS arm64, Rust 1.99.0, shared with other
builds. With four nextest workers on the rebased tree, all nine phase tests
passed in the workspace integration run. SDK scenarios took 100 ms to 1.10 s;
the receiver-side ninth-request test took 144 ms. Earlier alone remeasurements
also met 2 s, including custody partition catch-up at 1.01 s. No phase test
needed an exception. In-memory test secret providers avoid repeated key-file
I/O; runtime SQLite and identity persistence remain real. Metadata exchanges
skip identity and secret-store writes when no state changed.

The full check exposed two integration regressions addressed here: backed-off
explicit passes must report failure rather than success, and a checkpoint
that already supplied the command head still needs an applied acknowledgment
to its executor. The existing bootstrap and command tests retain those gates.
The bootstrap authority-recovery and borrowed-iroh ownership fixtures signal
restored connectivity before
retrying its backed-off contact; it still checks all rows and endpoint lifetime.
Kotlin core checks passed before the phase 18 rebase, including the iOS
simulator callback and lifetime tests; phase 18-specific Keychain and lifecycle
gates were not repeated here. No UniFFI ABI change was needed.

Final verification on `4f43c453` plus this change:

- `NEXTEST_TEST_THREADS=4 ./scripts/check`: exit 0; strict workspace Clippy,
  135 unit tests and 769 integration tests across the workspace and SDK without
  default features. Integration wall time was 128.23 s, within 5 min.
- `./scripts/lint`: exit 0; formatting, Kotlin, shell and workflow checks.
- `./scripts/commitlint --jj`: exit 0; the current Conventional Commit description.

## Boundaries

The fixtures qualify admitted in-process sessions and local partner choice.
No network discovery, WAN/relay, physical phone, hosted CI run or cryptographic
audit is claimed. Phase 15 owns publishing and resolving topics. Holdings and
selection reasons remain memory-only; no replicated roster is introduced.
The existing UniFFI ABI is unchanged: Kotlin peers map the new JSON fields and
contact requests use the existing generic call boundary.
