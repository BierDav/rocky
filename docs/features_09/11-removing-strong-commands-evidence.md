# Phase 11 removal evidence

Status: in progress; final acceptance checks pending. Work remains in
`xlvvwknokokz`, based on `woxunxwp`, in workspace `rocky-02`.
No Phase 10 recovery implementation was imported.

## Implementation and takeover

The takeover began with `jj status` and `jj diff`. The inherited change covered
180 files and removed about 29,000 lines: command contracts, trusted execution,
encrypted command storage, coordinator operations and command-only tests.
Those removals were retained and repaired, rather than assumed to compile.

The takeover completed SDK `DomainLog` composition, trusted-node registration,
control and closure; removed stale result sync, handler-generation metadata and
registry command identities; converted ordinary callers; and removed remaining
command-only tests, desktop publication controls and benchmark command streams.
The employee demo now uses ordinary encrypted row bundles. Benchmark results
from earlier command mixes remain historical; no new performance is claimed.

Native/Kotlin now compose service bundles without command handlers or tickets.
`DevelopmentLog` exposes the real in-process coordinated-log machine for local
fixtures, with the ledger's process lifetime explicit. The Kotlin coordinated
SQL helper's accidentally recursive `nextId` call was fixed.

Shopping requests are ordinary rows. Trusted devices read requests and existing
decisions inside coordinated transactions; the first committed decision stands.
The new acceptance test races opposite decisions from two trusted devices and
checks one decision on both and a member, which can ask but cannot decide.
Market restock reads and updates ordinary stock in one coordinated transaction;
its Room test verifies committed quantities and observer invalidation.

The inherited `Limits` retained six unused command-receipt fields solely to keep
old genesis signatures valid. That compatibility remnant was removed. Current
bootstrap signatures are `rocky.genesis.v2`, trust bundles format 2, controls
`rocky.control.v2`, Raft stores/snapshots format 4, manifests `rocky.manifest.v2`,
transitions `rocky.transition.v5`, backups `rocky.backup.v2`/inventory 2, encrypted files `rocky.encrypted-replica.v20`,
live links `rocky.link.v4`, and the trusted runtime SDK contract version 2.
Removed classes, command descriptors and obsolete formats are refused. Backups
carry only identity and SQLite; a regression verifies that an old receipt cache
is omitted and a retired archive is refused.

The published format-1 provider certificate is retired, retained as historical
information in `PUBLIC_COORDINATOR.md` and covered by a refusal regression.
A deployment supplies a newly provisioned current bundle; the shopping app has
no coordinator by default. No replacement public certificate or deployment is
fabricated. Ordinary requests remain writable without coordination.

The replacement guides, module map, README, optimization index, SDK composition,
public coordination and generation documentation follow the new contracts.
Decision 0073 documents command removal and explicitly leaves recovery to Phase 10.

## Verification log

Heavy builds and tests run sequentially. Logs below are under ignored `target/`.
A completed intermediate check does not qualify later source edits.

| Command | Result |
| --- | --- |
| Initial `./scripts/check --unit` | FAIL, exit 2; deleted coordinator binaries remained in tier filters |
| Initial combined package test, including SDK | FAIL, exit 101; SDK referenced removed command contracts |
| `cargo check -p rocky-sdk` during repair | FAIL; stale coordinator/handler/result fields and methods |
| `cargo test -p rocky-core -p rocky-credentials -p rocky-storage -p rocky-runtime -p rocky-secure -p rocky-coordinator -p directory-app -p inventory-app -p employees-app -p todo-app` | PASS, exit 0; `phase11-foundation-tests.log`, before later bootstrap changes |
| First `cargo test -p rocky-sdk` | FAIL, exit 101; five stale test API uses; `phase11-sdk-tests.log` |
| Second `cargo test -p rocky-sdk` | FAIL, exit 101; obsolete domain assertion; `phase11-sdk-tests-retry.log` |
| Third `cargo test -p rocky-sdk` | INTERRUPTED during the 100,000-row migration; no final result; `phase11-sdk-tests-final.log` |
| `cargo check --manifest-path sdks/native/Cargo.toml` | PASS, exit 0; `phase11-native-check.log`, before later bootstrap changes |
| `cargo check --workspace --all-targets` | PASS, exit 0 after stale smoke-test assertions were removed; `phase11-workspace-compile.log`, before later bootstrap changes |
| `sdks/kotlin/gradlew -p sdks/kotlin :rocky-core:compileKotlinJvm` | PASS; `phase11-kotlin-compile.log` |
| First `sdks/kotlin/gradlew -p sdks/kotlin check` | FAIL, exit 1; process-host fixture expected trusted SDK version 1 |
| `sdks/kotlin/gradlew -p sdks/kotlin check --max-workers=1` | PASS, exit 0, 4m41s; `phase11-kotlin-check-final.log`, before later bootstrap changes |
| `sdks/kotlin/gradlew -p sdks/kotlin publishToMavenLocal --max-workers=1` | PASS, exit 0, 3m22s; final republish needed after bootstrap changes |
| `sdks/kotlin/gradlew -p sdks/kotlin check publishToMavenLocal --max-workers=1` | PASS, exit 0, 6m03s; `phase11-kotlin-and-publish-final.log` |
| `./scripts/lint` | Intermediate passes; one later extra blank line failed and was repaired; final result pending |
| `scripts/commitlint --jj` | FAIL, exit 1; an ancestor has an empty description |
| `scripts/commitlint --jj @` | PASS, exit 0 for this change |
| `scripts/commitlint --jj 'trunk()..woxunxwp'` | FAIL, exit 1 with the same ancestor error |

The passing intermediate Kotlin run includes `CoordinatedRowsTest`: one test,
zero failures, 1.398 seconds in its JVM report. It also passed service-owner,
platform and packaged-consumer checks. The final shopping gate passes all 58 JVM and 25 Android tests, including
`ApprovalsTest.concurrentDecisionsConvergeAndMembersOnlyAsk`, plus required-schema
installation and desktop/Android builds (`phase11-shopping-check-complete.log`).

## Failure output and baseline attribution

The initial unit failure was:

```text
operator didn't match any binary IDs
rocky-coordinator::applications
rocky-coordinator::public_runtime
test-tiers: listing the tests of workspace failed with exit 94
```

The inherited SDK failures included unresolved `crate::commands`,
`rocky_core::Coordinator`, `CommandHost`, `Activation`, `ObservedCoordinator`,
`Protocol::Command` and old `serve_with_contacts`/`serve_all` argument counts.
The obsolete domain test failed with:

```text
unknown variant `command_controlled`, expected `mergeable` or `local_only`
```

The Kotlin failure was `Check failed` at `ProcessTransport.kt:100`, where the
fixture asserted version 1. Both request/reply and greeting checks now require
the trusted contract's current version 2. A lint failure was
`Activity.kt:185:1: Needless blank line(s)` and was repaired.

`jj file show -r woxunxwp` and `jj file list` confirmed the baseline contains
both deleted coordinator test binaries, the old command APIs and the matching
version-1 process-host contract. These were introduced intermediate removal
failures, repaired in this change. This is source attribution, not a claim that
the baseline test suites were executed and passed.

The commitlint failure is:

```text
commitlint: zllmttwwuyrz: the message is empty
```

It was reproduced by executing commitlint against `trunk()..woxunxwp` without
rewriting that ancestor. The current change's own description passes.

## Pending completion

- Finish the final Kotlin check/publication, then execute the shopping gate and
  its concurrent-decision acceptance test.
- Run package tests on final source and complete `./scripts/check`; native
  checks and `./scripts/check --unit` now pass. Finish
  `./scripts/lint` and `./scripts/check`; record exact final outcomes.
- Compare any unresolved check failures with `woxunxwp` and report limitations.

There is no Phase 10 implementation dependency for ordinary coordinated writes.
Owner-signed cuts and recovery beyond the hash window remain explicitly outside
this change. A production coordinator deployment needs a current signed bundle.
