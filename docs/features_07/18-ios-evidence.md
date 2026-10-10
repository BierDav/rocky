# Phase 18 evidence

Implementation: iOS Maven artifacts and static XCFramework, inventory validation,
independent consumer, device-only Keychain store, application notifications,
native foreground socket refresh/rebind and deterministic simulator fault probes.

Host: arm64 macOS, Xcode 27.0 (27A266a), iOS 27.0 arm64 simulator,
Rust 1.99.0, Kotlin 2.4.20 / Toolchain CLI 0.13.0. Minimum iOS: 13.0.

Rebased onto `main` at `ff86c729` without conflicts. Local software gates passed
on 2026-10-10; the physical completion gate remains blocked by signing.

## Ordered verification

| Command | Result | Wall time |
| --- | --- | --- |
| `./scripts/lint` | exit 0 | 3.44 s |
| `./scripts/check` | exit 0 | 277.22 s |
| `./kotlin check` | exit 0 | 308.59 s |
| `./sdks/native/check-ios` | exit 0 | 164.65 s |
| `./sdks/native/qualify-ios-sdk` | exit 0 | 168.67 s |
| `./sdks/native/qualify-ios-lifecycle` | exit 0 | 89.67 s |
| `./sdks/kotlin/package-ios` | exit 0 | 46.24 s |
| `./sdks/native/qualify-ios-package` | exit 0 | 64.29 s |

Rust: 135 unit tests and 752 integration tests passed, including the enforced
per-test and suite budgets. The integration run took 112.46 s.
The Kotlin check regenerated both bindings and passed the unchanged JVM/Android
lifecycle checks. The JVM stalled dial cancelled in 6 ms.
The iOS ABI gate passed both targets, each with and without iroh.

All seven new simulator tests passed their 2 s budget: committed reopen 257 ms,
notifications 111 ms, socket recovery 1,522 ms, stalled dial 517 ms, Keychain
update/delete 11 ms, secret-byte search/missing-key refusal 177 ms, and purge
621 ms. The direct simulator stalled dial cancelled in 0 ms and restarted in
1 ms. Each crash writer exited with the required code 87; before/inside/after
COMMIT recovered absent/absent/committed, with reconciliation matching rows.

Packaging produced the XCFramework and all three iOS Maven modules. The
inventory verified 44 distributed files. The independent consumer built for
the device and ran on the simulator without Cargo or rustup on PATH; its
integration test took 203.720 ms, below the enforced 2 s budget.

The Android prerequisite `publishToMavenLocal` passed (exit 0, 92.92 s).
`cargo clippy -p rocky-storage --features qualification -- -D warnings` and
`scripts/commitlint --jj` also passed. The final `./scripts/lint` rerun passed
(exit 0, 4.70 s), and the inventory still verified all 44 files after the consumer.
Raw transcripts and command results are
under the ignored `local/phase18-evidence/` directory; CI uploads its transcripts.

## Test placement

The Rust endpoint recovery and borrowed ownership regressions live in
`rocky-iroh`'s integration target. `scripts/test-tiers` includes them through
`kind(test)`; their budget remains 2 s per test. No exception was added.
The initial focused run passed four ownership tests in 0.05 s total.

Platform integration tests cover Keychain status handling, repeated store
objects, missing-secret refusal, byte searches, purge, notification ordering,
stalled dial cancellation and automatic sync after injected endpoint death.
The independent consumer exercises the published driver, durable writes and
reconciliation after reopen, with Cargo and rustup absent from PATH.

The crash probe normally checks committed-write reopen. Its subprocess script
starts separate simulator processes and exits without destructors before
sealing, inside SQLite COMMIT via its commit callback, and immediately after
COMMIT before flush/publication/reply. Reopen must report absent, absent and
committed respectively, with rows matching reconciliation. This qualifies
process death at commit boundaries, not power loss during a filesystem write.
The `qualification` feature enables these hooks only in contributor builds;
normal packaged runtimes omit them.

## Limits

No remotely hosted CI/release execution or publication is claimed. The
workflow definitions run the simulator gates and upload their transcripts.
Physical iPhone restart, locked-device Keychain behavior, restore refusal and
ten-minute background socket recovery are unverified. The host command
`security find-identity -v -p codesigning` reports `0 valid identities found`,
so a device-signed qualification application cannot be built on this host.
Personal-device applications and settings were left untouched. Phase 18's
physical completion gate is therefore blocked; simulator success does not close it.
