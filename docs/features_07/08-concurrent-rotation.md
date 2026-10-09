# 8. Concurrent rotation

Status: not started.
Owner: `rocky-secure` key epochs, frames and checkpoints; `rocky-credentials`
group secrets and envelopes; `rocky-sdk` rotation, renewal key lists and key
store. Depends on [phase 7](07-registry-revocation.md). Wave 2.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [equal issuers](scope-mesh.md#1-equal-issuers), the
[decisions](scope-mesh.md#decided-2026-10-09),
[key epochs and rotation](../runtime/credentials.md#key-epochs-and-rotation),
[retaining historical keys](../runtime/credentials.md#retaining-historical-keys),
[group secrets](../runtime/partitioned-scopes.md#group-secrets),
[decision 0040](../decisions/0040-group-secrets.md), and `install_key_epoch`
in `crates/rocky-secure/src/replica/keys.rs`.

A key epoch is a number per scope. Two issuers rotating one scope at once
both create the next number with different secrets, and the device that
receives the second is refused with "key differs from retained crypto
state". Let both exist, let writers converge, and close the exposure two
concurrent revocations leave.

## Settled

- Epoch identity: the generation plus a domain-separated hash of the secret.
  Both stand in the cleartext header. A higher generation supersedes; among
  siblings of one generation the lower hash wins.
- A sibling stays readable under the existing retention rule:
  `retire_key_epoch` drops it once no retained operation was sealed with it.
  The SDK retires nothing automatically. Siblings count against the 256
  epochs per collection and per group partition.
- An epoch records the IDs of the revocation statements its issuer held when
  it sealed it, not a digest.
- Exposure rotation: an issuer starts the next generation of a scope only
  when it holds a statement that is absent from the current epoch's recorded
  IDs and whose device could read that scope, by the selection of phase 7.
  A revocation of a device that could not read the scope triggers nothing.
- Termination is claimed only once revocations stop: issuers that then hold
  the same statements produce siblings recording the same IDs, which the
  tie-break settles. While statements still arrive, a further generation is
  expected.
- Two issuers creating one group partition at once are siblings of its first
  generation.
- The 0040 derivation does not change; siblings differ by their secret.

## Ordered slices

1. Decision record 0050: the identity, the order, the recorded statement IDs
   and every place that names an epoch: frames, bundle members, command
   parts, effect events, packages, the checkpoint statement's `key_epoch`,
   the recovery inventory, `secure_keys` and `secure_key_epochs`, the group
   secret envelope, the key lists of enrollment, renewal and `Supplied`,
   `identity.json`, and the secret names `reader.<collection>.<epoch>` and
   `group.<address>.<epoch>`.
2. Readers: a replica holds several siblings of a generation and opens what
   each sealed. `install_key_epoch` no longer refuses a different key for a
   known generation; it still refuses the same identity with other bytes and
   key material held under another scope. A frame under a sibling the
   replica lacks waits as pending.
3. Writers: a device seals under the winner among the epochs it holds. One
   holding only the loser keeps writing under it until the winner arrives.
4. Exposure rotation as settled, delivered only to devices the issuer's
   merged set admits. The guide states the window: from the concurrent
   revocations until the superseding generation reaches each writer.
5. A group partition's tables stay on one sibling together; an interrupted
   rotation or installation resumes to that sibling.
6. Checkpoints are sealed under the winner. One sealed under a loser or a
   superseded generation is not served once the winner is held.
7. Renewal and enrollment deliver every sibling of the current generation;
   `status` lists siblings and marks the one writers use.

## Completion gate

- [Acceptance 3](scope-mesh.md#acceptance) runs in CI on the in-process hub,
  for a collection key and for a group partition of three tables.
- Every device computes the same winner from the two identities alone, in
  both orders of arrival; a property test covers up to four siblings.
- Two issuers revoke two different readers of one list at once. After the
  issuers meet, a superseding generation exists that neither revoked device
  holds; neither opens a row written after it reached the writer. The test
  also lists the rows each could open in between.
- A revocation of a device that could not read a scope leaves that scope's
  generation unchanged on every issuer.
- Two issuers that hold the same statements and both start the superseding
  generation cause no further one: after they meet, no issuer rotates again
  in ten more sync rounds.
- Two issuers that created the same group partition while partitioned both
  open each other's rows after one sync.
- A writer offline through both rotations and the superseding one seals
  under the superseding generation after one renewal.
- Fault sweep over a group rotation, stopping after the intent is saved,
  after the secret is stored, after each table's key is installed and before
  the intent is cleared; and over installing a received sibling, stopping
  between tables: every table of the partition is on one sibling at reopen.
- At the `rocky-secure` level, `retire_key_epoch` drops a losing sibling once
  its operations are pruned and refuses before.
- The 257th epoch of a scope, siblings included, fails with the existing
  capacity error.
- [Credentials](../runtime/credentials.md) and
  [partitioned scopes](../runtime/partitioned-scopes.md) describe it.

## Not in this phase

- How an envelope reaches its recipient
  ([phase 12](12-direct-key-delivery.md)).
- A public verifier in the epoch identity: holding proofs toward a keyless
  custody peer use the partition tuple
  ([phase 10](10-holdings-after-authentication.md)).
- Automatic retirement of epochs in the SDK.
- Any change to the HKDF derivation or the meaning of a generation number.
- Sealing history again under the winner.
- Defending the tie-break against an issuer that chooses its secret; issuers
  are trusted ([not requested](scope-mesh.md#not-requested)).
- Chain references in the frame
  ([phase 11](11-chains-by-reference.md)), which lands after this on the
  frame format.
