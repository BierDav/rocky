# 12. Direct key delivery

Status: implemented on 2026-10-10 with
[decision 0054](../decisions/0054-envelope-delivery.md); rebased onto phase 10
(`main` at `ff86c729`), not landed. Direct and mailbox delivery, registry-driven
rotation delivery, delegated member sharing, private status, revocation cleanup,
restart/expiry and deterministic crash recovery are implemented. Qualification
and tier measurements are in [the evidence](12-direct-key-delivery-evidence.md).
Owner: `rocky-credentials` envelopes; `rocky-sdk` groups, sharing and
rotation; `rocky-secure` session; Kotlin `partitions`; the sharing code of
the shopping example. Depends on [phase 8](08-concurrent-rotation.md) and
[phase 9](09-roster-free-admission.md); lands after
[phase 10](10-holdings-after-authentication.md) on the session. Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [key delivery](scope-mesh.md#5-key-delivery), what was
[decided](scope-mesh.md#decided-2026-10-09), the envelope primitive in
[partition credentials](../runtime/partition-credentials.md#group-secrets),
the lifecycle in [partitioned scopes](../runtime/partitioned-scopes.md#group-secrets)
and the `keys` table in `ShoppingSchema.kt`.

`share` returns one sealed envelope per recipient and the application carries
them. The shopping example writes them to a replicated `keys` table that
every member reads, so every member sees who received which list and when.
After a revocation the application must share the new epoch itself. Deliver
envelopes to their recipient alone, inside the SDK.

## Settled

- A mailbox is any admitted peer whose chain carries a custody grant, and
  every issuer. It is independent of the partition.
- An envelope is addressed by the recipient's node key, which the mailbox
  sees. `GroupSecretEnvelope` names its group and recipient in the clear, so
  delivery wraps it in an outer sealing to the recipient's node key that
  hides the group, the epoch and the secret. The inner envelope keeps its
  checks and takes the epoch identity of phase 8.
- A mailbox learns that a recipient was sent something, when and how large.
  It learns no partition. The guide says so.
- The sender of an envelope deposits it: an issuer after a rotation, or
  whoever called `share`, a member with a delegated grant included. A deposit
  travels one hop: sender to mailbox to recipient.
- Bounds: 256 deposits per recipient and 4,096 per mailbox, each at most the
  4 KiB envelope bound plus the wrapping; a deposit expires 30 days after it
  arrived. A sender deposits one envelope with at most three mailboxes.
- Rotation recipients come from the registry: every unrevoked device whose
  record reads the partition.
- Collection reader keys stay with renewal.

## Ordered slices

1. Decision record 0054: the wrapped delivery, the deposit and its
   acknowledgment, who may hold them and the bounds above.
2. Outbox: `share` and a rotation save their pending envelopes durably with
   the intent in `identity.json`, before any network call. A rotation takes
   its recipients from the registry and needs no application call.
3. Direct delivery: one new session message family. After admission a
   session with the recipient hands over its pending envelopes; the
   recipient verifies and installs as today and returns an acknowledgment
   signed by its node key over the envelope digest; the sender then drops
   the envelope.
4. Mailbox: a sender deposits a wrapped envelope with a mailbox it meets.
   The mailbox stores it by recipient node key and digest and serves it only
   to the admitted session of that node key. It keeps the recipient's signed
   acknowledgment in place of the envelope and hands it to the depositor at
   their next session; both then forget it.
5. Revocation: a sender drops pending envelopes for a recipient in the
   revocation set. A mailbox removes that recipient's deposits when it learns
   the statement and serves none.
6. The surface: `share` returns no envelope; `status` reports each recipient
   as pending, deposited or delivered, to the sharer alone. `install` with a
   supplied envelope is removed. The shopping example stops writing and
   reading `keys` rows; the table and the grants on it stay until
   [phase 20](20-shopping-without-a-roster.md).
7. Update both group-secrets guides.

## Completion gate

- [Acceptance 8](scope-mesh.md#acceptance) is a CI run: a share and the
  envelopes of a rotation reach a recipient that was offline at the time and
  afterwards met only the custody peer.
- A third member's replica and session transcripts contain nothing that links
  the recipient to the partition; the mailbox's stored deposit names no group.
- A mailbox serves a deposit only to its recipient. At either bound it
  refuses with a retryable named error and the sender keeps the envelope.
- An envelope delivered directly and through two mailboxes installs once.
- A crash sweep over every step from `share` to the sender's receipt of the
  acknowledgment, on sender, mailbox and recipient, loses no envelope.
- A CI run shows a recipient revoked after the deposit receiving nothing
  from a mailbox that holds the statement, and the deposit removed.
- A restarted mailbox keeps its deposits; an expired one is gone.
- The shopping checks pass with no row in `keys`.
- [Partitioned scopes](../runtime/partitioned-scopes.md#group-secrets)
  describes delivery and what a mailbox learns.

## Not in this phase

- Removing the `keys` table and the member grants on roster tables
  ([phase 20](20-shopping-without-a-roster.md)).
- Mailbox-to-mailbox forwarding, or a deposit made for another sender.
- Delivering collection reader keys or the registry key.
- An envelope that grants a right, or a grant that delivers a key.
- Replacing an older deposit by a newer epoch: each envelope is its own
  deposit.
- Push notifications or waking a recipient.
- Hiding the recipient, sizes or timing from the mailbox.
