# 6. Mergeable revocation set

Status: not started.
Owner: `rocky-credentials` policy, capabilities and renewal; `rocky-secure`
admission, hello, checkpoints, horizon and generations; `rocky-sdk`
revocation and renewal; native and Kotlin status. No dependency; shares
`identity.json` with [phase 5](05-issuer-registry.md), in either order, the
second rebasing. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [equal issuers](scope-mesh.md#1-equal-issuers), the
[decisions](scope-mesh.md#decided-2026-10-09),
[revocation in the SDK](../runtime/credentials.md#revocation-in-the-sdk),
every entry of [limits](../runtime/credentials.md#limits), and
`SignedPolicy` and `CredentialChain::links` in `rocky-credentials`,
`learn_policy` in `rocky-secure`, `begin_revocation` and `rotate` in
`crates/rocky-sdk/src/enrollment.rs`.

The policy is one statement signed with the publisher key: an epoch number
and a revoked set. A capability is current only when its epoch equals the
policy's, so a revocation ends every chain; two issuers signing the same next
epoch are refused as "policy epoch equivocation". Replace it by a set of
statements that merges.

## Settled

- A revocation is one statement: the revoked node, user, capability IDs and
  pseudonym keys, the issuer's node key and a time. It is signed under the
  publisher authority and co-signed by the issuer's node key, for attribution
  only. It counts also when its issuer is itself revoked; two issuers revoking
  each other are both revoked.
- The policy a device holds is the set of statements it verified. Merging is
  union; a statement's ID is its hash; a digest identifies a set.
- Frame rule: a frame is refused when its author's node, user, capability or
  pseudonym is in the set, or its chain expired. A statement's time never
  enters admission. Rows of a revoked author that a device did not hold
  arrive by checkpoint. Confirmed command results keep their exception.
- Bound: 1,024 live statements; `revoke` fails with a named error at the
  bound. A statement leaves the distributed set once the longest chain it
  ends has expired: statement time plus the maximum chain lifetime (366 days)
  plus skew, by a day-rounded cutoff the digest also uses. What stops the
  revoked user renewing afterwards is the issuers' permanent tombstone.
  Stated limit: an issuer cut off from every other issuer for longer than
  that lifetime may readmit the user.
- Nothing forces a renewal any more. Until
  [phase 9](09-roster-free-admission.md) the hello carries the set digest,
  and a mismatch is a named refusal that feeds the existing renew-once path
  of sync.
- A key epoch is the scope's held maximum plus one, no longer the policy
  epoch plus one.
- No interim refusal of concurrent revocations. Until
  [phase 8](08-concurrent-rotation.md) two issuers rotating one scope at once
  still end in today's "key differs from retained crypto state" where the
  second key arrives. This phase tests concurrent revocations of devices
  reading disjoint scopes, and the guide says so.

## Ordered slices

1. Decision record 0049: statement, set, digest with its cutoff, bound, and
   the list of slice 3.
2. `rocky-credentials`: the statement, its two signatures, verification
   against publisher and manifest, the set with union, digest and expiry. A
   statement with a bad signature, for another domain or manifest, or over a
   size limit is refused and changes nothing.
3. Remove the policy epoch everywhere it reaches: capability and pseudonym
   credentials (the `epoch` field and its equality in `links`), group
   creation, the horizon statement's `policy_epoch`, the checkpoint
   statement's, the existence base, `secure_generations`, command history's
   synthesized statement and the bootstrap intent token. Each takes the set
   digest, or nothing where the epoch only expressed "current".
4. `rocky-secure`: `learn_policy` merges instead of comparing epochs; chains
   and frames are admitted by the frame rule; the hello carries the digest.
5. `revoke(node)` signs a statement and rotates over the issuer's records:
   the registry when [phase 5](05-issuer-registry.md) has landed,
   `identity.json` otherwise. The saved intent records the statement and each
   scope's target epoch. "Another revocation is unfinished" stays on one
   issuer; "one issuer revokes at a time" is removed from the guides.
6. Renewal answers and the `rocky.sdk.policy.v1` request carry the set.
   `revoked.json` takes its next version holding the statement that revokes
   this device; readiness keeps its meaning. Renewal refuses a tombstoned
   user whatever statements remain.
7. Native and Kotlin status replace `policyEpoch` by the digest and the live
   statement count. Guides lose every mention of equivocation.

## Completion gate

- [Acceptance 1](scope-mesh.md#acceptance) runs in CI on the in-process hub:
  two partitioned issuers each revoke a device, the two reading disjoint
  scopes; joined in both orders, every device holds both statements and
  neither is refused.
- A property test merges the same statements in every interleaving and gets
  one digest; statements past the cutoff do not change it.
- A device that learned a revocation refuses the revoked author's new frames
  and admits frames it lacked from other authors, including ones signed
  before the revocation.
- A hello with a differing digest is refused with the named error, and the
  refused member holds the statement after its one renewal.
- A revoked user cannot renew under a fresh node key at any issuer, also
  after its statement left the set.
- The 1,025th live statement fails with the named error and rotates nothing.
- Fault sweep over `revoke`, stopping after the intent is saved, after each
  new key is made, after the statement is signed, after the issuer's own
  chain is renewed and before the intent is cleared: the next open completes
  with the same statement ID and the same keys.
- No test or guide mentions equivocation between honest issuers.

## Not in this phase

- Revoking precisely a device another issuer enrolled
  ([phase 7](07-registry-revocation.md)); an unknown device still rotates
  everything.
- Two rotations of one scope ([phase 8](08-concurrent-rotation.md)); no
  refusal is added for them.
- Sending statements inside a session, refusing a revoked peer's session
  beyond the digest refusal, and admission without a contact
  ([phase 9](09-roster-free-admission.md)).
- Ordering statements, comparing their times, or recovery from a stolen
  issuer ([not requested](scope-mesh.md#not-requested)).
- Sealing history again or retracting what a revoked device decrypted.
