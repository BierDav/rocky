# 7. Revocation from the registry

Status: not started.
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
`crates/rocky-sdk/src/enrollment.rs` with `Holder::selects` in `groups.rs`.

After phases 5 and 6 an issuer signs a mergeable statement and reads device
records from the registry, but `rotate` still treats a device this issuer did
not enroll as one that "may have read anything" and rotates every collection
and every known group partition. Make any issuer revoke any device with the
precision of the enrolling one.

## Settled

- What rotates is what the registry record selects: every collection the
  profile reads and every group partition whose members the profile reads or
  a recorded partition grant selects.
- Delegated grants made on a non-issuer stay unrecorded. Revoking a node with
  no registry record rotates everything, as today. Revoking a recorded device
  also rotates what its delegator's record selects, when its chain shows one.
- A recorded grant with a delegation depth above zero means the device may
  have delegated. Its delegates have no record and are reached only by the
  rule above when they are revoked themselves; revoking the delegator does
  not revoke them. The guide states this limit.
- The statement names the node, the user, the latest capability ID and every
  pseudonym key the record lists, whichever issuer assigned them.
- Revoking an issuer rotates the registry key like any reader key, because
  its profile reads the registry.
- The tombstone is written to the registry in the same local transaction that
  completes the revocation. It is what refuses renewal once the statement
  has left the set.
- A device already tombstoned, or already named by a held statement, is
  "device is already revoked"; two issuers that revoke one device at once
  both succeed and leave two statements and one tombstone.

## Ordered slices

1. `rotate` selects scopes from the registry record instead of this issuer's
   own knowledge. Remove the "never saw it" branch for recorded devices and
   keep it, named as such, for a node with no record.
2. The delegator rule: resolve the revoked device's chain to the recorded
   device that delegated to it and add what that record selects.
3. The statement's subjects come from the record: user, capability ID and
   pseudonym keys across every table and every assigning issuer.
4. The tombstone and the intent: the saved intent names the statement and
   each scope's target epoch as in phase 6; completion sets the tombstone
   and clears the intent together.
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
- A device delegated to by a recorded member is revoked: the delegator's
  selected partitions rotate, the delegator is not revoked.
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
- Delivering the new key to remaining readers other than by renewal
  ([phase 12](12-direct-key-delivery.md)).
- Refusing the revoked device's session
  ([phase 9](09-roster-free-admission.md)).
- Device management screens in shopping
  ([phase 20](20-shopping-without-a-roster.md)).
