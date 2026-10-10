# 7. Revocation from the registry

Status: implemented on 2026-10-10 under the revised unrecorded-device
contract below; the hub acceptance and twelve-point fault sweep pass.
[Qualification evidence](#qualification-evidence) records the checks and limits.
Owner: `rocky-sdk` revocation and rotation; Kotlin `revoke` and `devices`.
Depends on [phase 5](05-issuer-registry.md) and
[phase 6](06-revocation-set.md). Changes no format and takes no decision
record. Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [equal issuers](scope-mesh.md#1-equal-issuers), the
[decisions](scope-mesh.md#decided-2026-10-09),
[revocation in the SDK](../runtime/credentials.md#revocation-in-the-sdk), the
revocation paragraph of
[group secrets](../runtime/partitioned-scopes.md#group-secrets), and
`begin_revocation`, `rotate` and `revoked_device` in
`crates/rocky-sdk/src/enrollment.rs` with `Revoking::selects` in `enrollment.rs`.

After phases 5 and 6 an issuer signs a mergeable statement and reads device
records from the registry, and selects scopes from those records. Complete
precise revocation at any issuer, including partition-only records and durable tombstone completion.
Keep full rotation for unrecorded devices.

## Settled

- What rotates is what the registry record selects: every collection the
  profile reads and every group partition whose members the profile reads or
  a recorded partition grant selects.
- Delegated grants made on a non-issuer stay unrecorded. Revoking a node with
  no registry record rotates every held collection key and every known group
  partition whose secret this issuer holds. No delegator is inferred or
  revoked. A recorded device without a profile selects only its recorded
  partition grants, not every collection.
- A recorded grant with a delegation depth above zero means the device may
  have delegated. Its unrecorded delegates use the full-rotation fallback
  when revoked individually. Delegator revocation does not enumerate their devices or
  rotate additional scopes on their behalf; shared chain ancestors may still
  end their credentials. The guide states this limit.
- The statement names the node, the user, the latest capability ID and every
  pseudonym key the record lists, whichever issuer assigned them.
- Revoking an issuer rotates the registry key like any reader key, because
  its profile reads the registry.
- The registry tombstone transaction is the durable completion marker.
  The existing JSON intent is a redo record and is discarded only after that
  transaction commits, before returning success. Open resumes either side
  of the commit with the same statement and keys. The two stores have no
  cross-file atomic write; a committed tombstone makes a stale intent
  cleanup, not another revocation. The tombstone refuses renewal once the
  statement has left the set.
- A device already tombstoned, or already named by a held statement, is
  "device is already revoked"; two issuers that revoke one device at once
  both succeed and leave two statements and one tombstone.

## Ordered slices

1. `rotate` selects scopes from the registry record instead of this issuer's
   own knowledge. Remove the "never saw it" branch for recorded devices and
   keep it, named as such, for a node with no record.
2. Distinguish a partition-only record from an absent record. Only the latter
   uses full rotation, including an unrecorded delegate.
3. The statement's subjects come from the record: user, capability ID and
   pseudonym keys across every table and every assigning issuer.
4. The tombstone and the intent: the saved intent names the statement and
   each scope's target epoch as in phase 6; the tombstone commits completion
   before the redo intent is discarded. Open completes this boundary before
   sessions or the network can serve work.
5. Kotlin and native `revoke` accept any node the registry lists; `devices()`
   shows the tombstone on every issuer after one sync.
6. Guides: credentials, enrollment and partitioned scopes describe the
   selection, the unrecorded-delegation limit and the removal of "every
   collection if this issuer never saw the device".

## Completion gate

- [Acceptance 2](scope-mesh.md#acceptance) runs in CI on the in-process hub:
  B revokes a writer of one collection that A enrolled, A being stopped.
  Only that collection and the group partitions the record selects get a new
  epoch; every other scope keeps its epoch; the device opens no row written
  after the writer received the new key.
- The same run with a device holding one recorded partition grant rotates
  that group partition and no other.
- Revoking a node absent from the registry rotates every collection and
  every known group partition, and the test names that as the fallback.
- An unrecorded delegate of a recorded member uses the full-rotation
  fallback; the recorded member remains authorized.
- A pseudonym that issuer A assigned is in the statement B signs, and frames
  under it are refused by a device that learned the statement.
- Revoking an issuer gives the registry a new key epoch, which the revoked
  issuer does not receive at any remaining issuer.
- After B's revocation and one sync, A lists the device as revoked and
  refuses its renewal; so does A when the statement is removed from its set
  by the test clock passing the cutoff.
- Fault sweep, stopping after the intent is saved, after each new key is
  made, after the statement is signed and before the tombstone transaction
  commits: the next open completes with the same statement ID, the same keys
  and one tombstone.

## Not in this phase

- Any format: the statement, the registry record, `identity.json` and the
  renewal answer are those of phases 5 and 6. A missing field is reported as
  a blocking contract, not added here.
- Recording delegations made on a non-issuer, or revoking a delegator's
  delegates with it.
- Two issuers rotating one scope at once
  ([phase 8](08-concurrent-rotation.md)). Tests here revoke from one issuer
  at a time per scope.
- Adding automatic delivery of new keys to remaining readers
  ([phase 12](12-direct-key-delivery.md)); existing group-secret share/install
  remains the manual delivery path.
- Refusing the revoked device's session
  ([phase 9](09-roster-free-admission.md)).
- Device management screens in shopping
  ([phase 20](20-shopping-without-a-roster.md)).

## Contract revision on 2026-10-10

The user chose full rotation for unrecorded devices after verification of
landed phases 5 and 6 showed that `revoke(node)` has no credential chain and
the fixed registry has no delegation provenance. Non-issuer delegation
returns a chain but stores no recipient relationship. Phase 7 therefore
requires no ancestry lookup: recorded devices select their recorded access;
unrecorded devices use the explicit full-rotation fallback. Durable provenance
is deferred to [an idea](../ideas/delegation_provenance.md).

## Qualification evidence

`cargo test -p rocky-sdk --test registry_revocation` passes five hub scenarios:
precise revocation with the enroller stopped, partition-only selection,
unrecorded-delegate full rotation with its delegator still able to renew
but refusing fresh delegation to the revoked node,
issuer registry-key rotation and concurrent statements for one non-reader.
The cross-issuer scenario checks A's pseudonym in B's signed statement and
refusal by the incoming-frame credential verifier. After A returns, the
existing share/install calls supply its rotated group secret before one sync
learns the tombstone; collection keys arrive through renewal.

`cargo test -p rocky-sdk --lib node::revocation_tests` passes the twelve-point
fault sweep through each collection key and group secret, statement signing,
self-renewal, rollback before tombstone commit, committed tombstone before
cleanup and intent clearing. Every real database reopen has the same statement
ID and keys, exactly one tombstone and no remaining intent; a second reopen
adds no epoch. Enrollment tests prune statements beyond the cutoff and still
refuse revoked-user renewal at both issuers. The existing eight group-secret
tests pass with the partition-only precision expectation updated.

`./scripts/check` passes formatting, strict workspace Clippy, all 714
workspace tests (15 skipped), three doc tests and all 165 SDK tests without
default features (4 skipped). The final delegation-refusal assertion also
passes under default features, with a fresh strict SDK Clippy check.
Native ABI checks (including two tests and the configuration without default
features) and Kotlin `rocky-core` builds for JVM and Android pass.
`./scripts/lint` and `./scripts/commitlint --jj` pass. Hosted CI, ignored soaks
and iOS/device
qualification have not been run here. Concurrent rotations of one scope and
automatic group-key delivery remain phases 8 and 12.
