# Phase 12 delivery evidence

Measured on 2026-10-10 in the Phase 12 Jujutsu workspace, rebased onto
`main` at `ff86c729` (Phase 10). The host is an 18-core Apple M5 Pro,
shared with other builds. No remote CI run or physical-device run is claimed.

## Contracts and tiers

The native session is version 11; the identity file is version 13 and the
mailbox file is version 1. Earlier formats are refused. See
[decision 0054](../decisions/0054-envelope-delivery.md) for the protocol and
[partitioned scopes](../runtime/partitioned-scopes.md#group-secrets) for the
consumer contract and mailbox privacy.

- Unit: `rocky-credentials` delivery sealing, substitution/version refusal
  and recipient-signed receipt checks. No files, sockets or runtimes.
- Integration: `rocky-secure`'s real authenticated delivery session;
  `rocky-sdk`'s offline share plus registry-driven rotation (acceptance 8),
  recipient-only serving, recipient capacity refusal, direct plus two-mailbox
  deduplication, revoked-recipient cleanup, independent custody grants,
  delegated sender authority and sharer-only status. These use the existing
  package filters and the 2 s budget; no exception is introduced.
- E2e: the nine deterministic delivery crash cases and the full 4,096-deposit
  capacity/restart/expiry case, explicitly selected by
  `.config/test-tiers.toml`. The crash cases restart the sender, mailbox or
  recipient after checkpoint intent, installation, deposit storage, deposit
  acknowledgment, receipt storage or receipt forgetting. All retries preserve
  the envelope, installation and delivery outcome.

Reader and writer profiles include custody and therefore qualify as mailboxes.
The ordinary third-member privacy fixture uses raw read-only grants without
custody: its replica and all recorded session bytes contain no recipient node
key. The mailbox stores the node key but no group name, values or address.

## Verification

`scripts/test-tiers e2e --build='-p rocky-sdk --lib'`: ten tests passed;
nextest execution 3.009 s, tier wall 18.55 s including a rebuild,
18.55 s after load scaling, under the 15 min budget. The lost deposit-answer
case also asserts the target reservation is on disk before restart.

`./examples/shoppinglist-app/check`: required schema installation, all 28 JVM
tests and desktop build passed. Sharing from a non-issuer is checked after it
receives the key; issuing credentials and revoking remain issuer-only.

`./examples/shoppinglist-app/qualify`: all four multi-process scenarios passed,
using the packaged custody launcher and Kotlin applications. The refreshed
gate overlapped native builds and the shared Rust checks:

| Scenario | Time | Key table check |
| --- | ---: | --- |
| gate | 70.662 s | no rows on three devices |
| exclusion-revocation | 7.767 s | no rows on three devices |
| relink | 6.032 s | no rows on three devices |
| creator-loss | 6.954 s | no rows on two devices |

The local qualification output is `local/shopping-qualification/evidence.json`.
The table and its grants remain for Phase 20, but no application path writes
or reads envelope rows.

The first Rust check encountered an unrelated unit-test budget miss during
heavy concurrent builds (load 75.8 on 18 cores). The pagination test that
missed the unit budget creates a real SQLite file and runs Tokio; its classification is corrected to integration, with no
budget change.

`./sdks/native/check`: exited 0, including strict ABI Clippy, the ABI tests
and the build without default features. Both Kotlin bindings are regenerated
by the shopping/iOS workflows.

The iOS simulator SDK gate passed the common SDK, SQLite driver and Room
fixtures, endpoint ownership, Kotlin command callbacks, transport/secret-store
callbacks, cancellation and the 1,000-cycle lifetime test. At 1,000 cycles it
reported zero native handles and 129,712,128 B RSS, versus 124,125,184 B at 100. The refreshed
`./sdks/native/qualify-ios-sdk` run exited 0 on the final runtime code.

`./scripts/lint`: exited 0.
`./scripts/check`: exited 0, with strict workspace Clippy, 137 unit tests
(116.5 ms tier wall) and 767 integration runs across the workspace and the
SDK without default features (138.73 s tier wall). Every per-test and suite
budget passed; no new exception was added. The new member-mailbox and
multi-link delegated-sharing scenarios also passed their integration budgets.

`scripts/commitlint --jj` passed. There are no unresolved rebase conflicts;
`main` remains at `ff86c729`. The implementation remains in the current
Conventional Commit change and has not been pushed.
