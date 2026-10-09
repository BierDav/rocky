# 6. Concurrent rotation

Status: not started.
Owner: `rocky-secure` key epochs and frames; `rocky-credentials` group
secrets and envelopes; `rocky-sdk` rotation and key store. Depends on
[phase 5](05-mergeable-revocations.md). Wave 2.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [equal issuers](scope-mesh.md#1-equal-issuers), the
[proposed defaults](scope-mesh.md#proposed-defaults),
[key epochs and rotation](../runtime/credentials.md#key-epochs-and-rotation),
[group secrets](../runtime/partitioned-scopes.md#group-secrets) and
[decision 0040](../decisions/0040-group-secrets.md).

A key epoch is a number per scope; a different key for a known epoch is
refused. Two issuers rotating one scope at once both create the next number
with different secrets, so one of them is refused everywhere. Let both exist
and let writers converge.

## Ordered slices

1. Decision record for the epoch identity: a generation and a value that
   distinguishes siblings, such as the secret's fingerprint. Frames, bundle
   members, command parts, effect events, packages, envelopes and the key
   store name it. State the order: a higher generation supersedes; siblings
   of one generation are ordered by the tie-break.
2. Readers: a replica holds several siblings of a generation and opens
   whatever each sealed. A frame under a sibling it lacks waits as pending.
3. Writers: a device seals under the winning sibling it holds. A device
   holding only the losing one keeps writing under it until it receives the
   winner; both are readable, so nothing is lost. State the window in which a
   revoked device could still read: none, since it receives neither sibling.
4. Rotation by an issuer that sees two siblings starts the next generation
   from the winner. A group partition's tables stay on one sibling together;
   an interruption resumes to that.
5. Retirement: a losing sibling is retired under the existing rule.
   Checkpoints are sealed under the winner; one sealed under the loser is not
   served once the winner is held.
6. The refusal added by [phase 5](05-mergeable-revocations.md) is removed.

## Completion gate

- [Acceptance 3](scope-mesh.md#acceptance) passes for a collection key and for
  a group partition of three tables.
- Every device computes the same winner from the two siblings alone, whatever
  the order of arrival.
- A device revoked by either of the two concurrent revocations opens no row
  sealed under either sibling.
- A writer offline through both rotations and a third one converges on the
  third generation after one renewal.
- A crash at every step leaves every table of a group partition on the same
  sibling.
- A losing sibling retires once no retained operation needs it.
- [Credentials](../runtime/credentials.md) and
  [partitioned scopes](../runtime/partitioned-scopes.md) describe it.

## Not in this phase

How an envelope reaches its recipient
([phase 11](11-direct-key-delivery.md)).
