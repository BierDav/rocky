# 5. Mergeable revocations

Status: not started.
Owner: `rocky-credentials` policy; `rocky-secure` admission; `rocky-sdk`
revocation and renewal; Kotlin status. Depends on
[phase 4](04-issuer-registry.md). Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [equal issuers](scope-mesh.md#1-equal-issuers), the
[proposed defaults](scope-mesh.md#proposed-defaults),
[revocation in the SDK](../runtime/credentials.md#revocation-in-the-sdk) and
every entry of [limits](../runtime/credentials.md#limits).

The policy is a signed statement with one epoch number and a revoked set. Two
issuers signing the same next epoch are refused as equivocation, receivers
reject every frame signed under an earlier epoch, and an issuer revokes
precisely only devices it recorded. Replace the linear policy by a set that
merges.

## Ordered slices

1. Decision record for the policy format. A revocation is one statement
   signed by an issuer: the revoked node, user and capability, the issuer and
   a time. The policy a device holds is the set of statements it verified.
   Merging is union; a digest identifies a set. Nothing is ordered and
   nothing is removed.
2. Verification: a statement counts when signed under the domain's publisher
   authority by a chain that is itself valid. State and test what happens to
   statements signed by an issuer that another statement revokes; the scope
   excludes [recovery from theft](scope-mesh.md#not-requested), not a defined
   answer.
3. Admission of chains and frames against the set, replacing the epoch
   comparison. Settle the frame rule of the proposed defaults; keep the
   exception for confirmed command results and the checkpoint path for rows
   a device did not yet hold.
4. `revoke(node)` from the registry: any issuer revokes any device, rotating
   only the collections and group partitions its record could read. The
   saved intent and its completion after an interruption stay. Two issuers
   may revoke at once; "one issuer revokes at a time" is removed.
5. Renewal answers and the public policy request carry the set, or the part
   the asker lacks. `revoked.json` and readiness keep their meaning.
6. Replace `policyEpoch` in the native and Kotlin status by the set's digest
   and size. Bound the set and state the bound.

## Completion gate

- [Acceptance 1 and 2](scope-mesh.md#acceptance) pass on the in-process hub,
  with the two issuers partitioned and joined in both orders.
- The merged set is identical on every device whatever the order of arrival;
  a property test covers interleavings.
- A statement with a bad signature, from a non-issuer or for another domain
  is refused and changes nothing.
- A device that learned a revocation refuses the revoked peer's session and
  new frames; honest frames it lacked from other authors are still admitted.
- A revoked user cannot renew under a fresh node key at any issuer.
- An interruption at every step of a revocation completes at the next open
  with the same statement.
- No test or guide mentions equivocation between honest issuers.

## Not in this phase

Converging the two key epochs two concurrent revocations create
([phase 6](06-concurrent-rotation.md)) and carrying the set in the session
handshake ([phase 7](07-roster-free-admission.md)). Until phase 6 lands, two
issuers revoking readers of the same scope at once is refused at the rotation
with a named error.
