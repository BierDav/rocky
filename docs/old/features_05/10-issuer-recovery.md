# 10. Issuer recovery

Status: slices 1–3 implemented through `rocky-sdk` enrollment, the native
ABI and the Kotlin starter layer; qualified on JVM desktop over memory and TCP
loopback; completion gate met. Owner: the Kotlin starter layer and `rocky-sdk`
enrollment. Depends on [phase 1](01-credential-primitives.md). Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 9, item 3](agreed-scope.md#9-platform-and-operations) of the
scope, [SDK enrollment](../runtime/sdk-enrollment.md) and the delegated
issuer rights of [features_04 phase 3](../features_04/03-credentials-and-enrollment.md).

If the creator's device is lost, nobody can invite or renew. With phase 1's
raw grants and delegation an application can grant issuing rights to a second
device. The starter layer does so by default, so a shopping-list user with two
devices or an [always-on peer](06-always-on-peer.md) is never locked out.

## Ordered slices

1. The starter pairing flow delegates issuing rights to the newly paired
   device by default, with an explicit opt-out; no cloned author logs.
2. Renewal and invitation through any issuing device; the original creator is
   not a required hub.
3. Loss of the creator: a remaining issuer revokes it by policy epoch and the
   domain continues; document what is unrecoverable when the last issuer is
   lost.

## Completion gate

- After the creator device is destroyed, a second device invites a third and
  renews an expiring credential.
- A device paired with opt-out cannot issue.
- The flow needs no Rust and is documented in
  [SDK enrollment](../runtime/sdk-enrollment.md).

## Measured evidence

Measured on 2026-10-08 in the dedicated phase-10 workspace. The mechanism is
the issuer profile of [decision 0023](../decisions/0023-credentials-profiles-and-key-epochs.md#amendment-issuer-recovery):
a chain delegated with phase 1's `delegate` cannot outlive its delegator's
validity or sign a policy epoch, so it cannot renew anyone or revoke the
creator after the creator is gone. The paired issuer holds the publisher key
under its own provider with its own keys and a split author range.

- Slice 1: Kotlin `pairDevice(issuing = true)` on `RockySession`,
  `RockyDatabase`; `issuing = false` pairs a writer of every table from the
  native `domain` call's new `collections`. A joining issuer sends its routes
  in the pair request, bound by the proof (`rocky-credentials`
  `a_joining_issuers_routes_are_bound_by_its_proof`); the enrolling issuer keeps
  them. `tests/issuers.rs` checks distinct principals and disjoint author ranges.
- Slice 2: enrollments, renewal answers and `grant` bundles carry the other
  issuers' contacts; a refused sync renews from the refusing issuer first.
  `issuers.rs` checks a late device learning the second issuer at enrollment,
  an early one at renewal, and the preferred renewal (it fails without it).
- Slice 3: `revoke` accepts another issuer and refuses the caller itself;
  devices forget peers the adopted policy revoked. The unrecoverable cases are
  in [issuer recovery](../runtime/sdk-enrollment.md#issuer-recovery).
- Gate, Rust: `cargo test -p rocky-sdk --test issuers --locked`, 2 passed:
  after the creator's directory and keys are deleted the second issuer revokes
  it (epoch 2), invites a third device that syncs, and renews a device the
  creator enrolled and one it granted for 60 seconds, to the 30-day lifetime.
- Gate, Kotlin: `IssuerRecoveryTest` in the shopping example, 2 passed through
  the generated native bindings over TCP: the same recovery with `pairDevice`,
  and an opt-out device refused for `pairDevice`, `pairing`, `grant`,
  `delegate` and `revoke`. No Rust is written by the application.
- `./scripts/check`: passed formatting, workspace Clippy with warnings denied,
  workspace tests and doc tests, and `rocky-sdk` without default features.
- `./sdks/native/check`, `./sdks/kotlin/check` and
  `./sdks/kotlin/check-packaged`: passed.
- `./examples/shoppinglist-app/check`: 27 Kotlin/JVM tests passed and the
  desktop build passed, after the stale identity-version assertion of the
  phase-1 pseudonym test was corrected from 7 to 8 in a separate change.

Qualification is bounded to JVM desktop over memory and TCP loopback; it is
not a cryptographic audit, and Android was not rerun for this phase.
