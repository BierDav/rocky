# Phase 4: held changes evidence

Status: implemented; all three completion gates passed (2026-10-14).

## Prerequisites and inherited implementation

Work continues in the default workspace change `xkykmxru`, parent `zllmttww`.
Phase 3's removal classification, signed lineage names, field-by-field
admission, projection switch and durable replay queue are present. Phase 6
removed cuts and retained manifests only while referenced. Those contracts
remain the basis of holding; no alternative migration path was introduced.

The inherited partial implementation provided never-offered tracking,
whole-bundle holding with dependent bundles, signed void storage/admission,
frontier and transport support, rollback by replay, Rust/JSON/Kotlin held
lists and atomic SQL re-authoring/drop. It also added runtime, SDK and Room
tests. Its initial three-package `cargo check` passed.

This continuation adds decision 0069, current-format handshake refusal,
held-ID binding in re-authoring handles, failure/restart/exposure regression
tests, Clippy type factoring and owning guide links. The encrypted file is v17 and the handshake v2;
there are no legacy readers or migrations.

## Gate coverage

| Gate | Implementation and tests |
| --- | --- |
| A never-offered kept/removed-table bundle is held, voided, rolled back; later edits reach peers | `rocky-secure` `tests::generations::held::an_unsent_bundle_a_removal_splits_is_held_voided_and_reauthored`; SDK `tests/removal.rs` `an_unsent_change_a_removal_splits_is_held_and_reauthored_after_the_update` |
| The list shows a peer's newer value and re-authoring wins everywhere | The same runtime and SDK scenarios; Room `anUnsentChangeTheNewerVersionRemovesIsHeldAndReauthored` qualifies the Kotlin mapping API |
| A void covering an offered frame is refused | Runtime gate; exposure without delivery and one offered member protecting a whole bundle additionally covered by `offering_one_bundle_member_without_delivery_prevents_holding_after_restart` |

The partial work already covered the three functional gates in tests; the
missing ordered slices were decision 0069, guide/status/evidence updates
and verification. Additional regressions check a missing held ID rolls back
fresh writes, different held IDs conflict under a reused handle, and
unresolved held values survive restart before dropping.
An injected activation failure checks that holding, voiding and projection
changes all roll back before a successful retry.

## Checks

- `cargo check -p rocky-core -p rocky-sdk -p rocky-secure`: passed initially.
- `./scripts/check --unit`: 157 passed, 84.6 ms wall at load 88.4 on 18 cores.
- `cargo test -p rocky-core -p rocky-sdk -p rocky-secure`: passed, including
  doctests; secure library 262 passed, 11 intentionally ignored.
- `./scripts/lint --fix` and `./scripts/lint`: passed.
- Kotlin Room generation tests: 2 passed, held-change gate 0.297 s.
- `./scripts/check`: passed on retry, including workspace Clippy, 174 unit
  tests (93.2 ms) and 1,015 integration tests (180.31 s, 300 s budget), with
  and without the SDK's default features. The SDK held-change gate reran
  alone in 818.8 ms against its 2 s budget.
- Private publication: passed with `-Dmaven.repo.local=/tmp/rocky-phase4-maven`.
- `sdks/kotlin/gradlew -p sdks/kotlin check`: passed on retry in 3m 27s,
  including JVM and simulator tests, Room and packaged qualification. The
  previously failing service-owner test passed in 19.732 s; the normal 180 s
  deadline is unchanged.
- `./examples/shoppinglist-app/check`: passed using
  `ROCKY_SHOPPING_PROJECT=/tmp/rocky-phase4-shopping` and the private Maven
  property below. The copied application differs only in repository selection;
  hand/Room schema equality and native installation, all 59 tests, desktop
  compilation and Android compilation passed.

### Failed attempts and corrections

Initial `./scripts/check` stopped in Clippy on the inherited tuple types:

```text
error: very complex type used. Consider factoring parts into `type` definitions
  --> crates/rocky-secure/src/replica/held.rs:45:36
error: very complex type used. Consider factoring parts into `type` definitions
   --> crates/rocky-secure/src/replica/held.rs:533:23
error: could not compile `rocky-secure` (lib) due to 2 previous errors
error: could not compile `rocky-secure` (lib test) due to 2 previous errors
```

Factored the verified-header and unsigned-log types without suppressing lints;
workspace Clippy passed on retry.

Initial Kotlin check failed in the unchanged service-owner fixture:

```text
Execution failed for task ':rocky-core:allTests' (registered by plugin 'rocky.multiplatform').
> There were failing tests. See the report at: file:///Users/david-bieregger/RustroverProjects/rocky-db/sdks/kotlin/rocky-core/build/reports/tests/allTests/index.html
java.lang.IllegalStateException: service command timed out
    at rocky.ServiceOwnerTest$closingAtTheTenantQuotaAllowsAnotherDomainAndRefusesANonOwner$1.invokeSuspend$command(ServiceOwnerTest.kt:27)
    at rocky.ServiceOwnerTest$closingAtTheTenantQuotaAllowsAnotherDomainAndRefusesANonOwner$1.invokeSuspend(ServiceOwnerTest.kt:34)
BUILD FAILED in 9m 22s
```

The failing line runs `cargo build --locked -p rocky-coordinator --bin
rocky-voter` under a 180 s deadline while concurrent Cargo jobs held the build
lock. No timeout was increased; the full Kotlin retry passed after the Rust
builds completed.

The requested change was described with the Claude co-author footer.
`scripts/commitlint --jj @` passes. The default ancestor-range check refuses
the untouched empty parent, which this task forbids rewriting:

```text
commitlint: zllmttwwuyrz: the message is empty
```

The first shopping attempt used an isolated project with a `file://` Maven
repository. Kotlin CLI 0.13 refuses that repository before compilation:

```text
WARN  Non-https repositories are not supported, skipping url: file:///tmp/rocky-phase4-maven
ERROR: Task ':qualification:resolveDependenciesJvm' failed: Unable to resolve dependencies for module qualification, see the errors above.
```

Retry uses the isolated project with `mavenLocal` and
`KOTLIN_CLI_JAVA_OPTIONS=-Dmaven.repo.local=/tmp/rocky-phase4-maven`;
the shared `~/.m2` repository is not written.

## Limits

This gate uses real SQLite and deterministic in-process peers; it is not a
real-network, cryptographic or mobile lifecycle qualification. A signature
cannot prove a malicious author never offered a frame to another copy;
conflicting first admissions remain author forks. Existence records of held
creations remain. Recovery packages do not carry voids or held images and
reject incomplete frame closure; backup/range/checkpoint paths are distinct.
Long-running void retention bounds and exhaustive power-loss fault sweeps
remain unproven. No Phase 12 shopping removal qualification is claimed.
