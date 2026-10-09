# 11. Direct key delivery

Status: not started.
Owner: `rocky-credentials` envelopes; `rocky-sdk` groups, sharing and
rotation; `rocky-secure` session; Kotlin `partitions`. Depends on
[phase 6](06-concurrent-rotation.md) and
[phase 7](07-roster-free-admission.md). Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [key delivery](scope-mesh.md#5-key-delivery), the
[proposed defaults](scope-mesh.md#proposed-defaults),
[group secrets](../runtime/partition-credentials.md#group-secrets) and the
shopping example's `keys` table in `ShoppingSchema.kt`.

`share` returns one sealed envelope per recipient and the application carries
them. The shopping example writes them to a replicated `keys` table that
every member reads, so every member sees who received which list and when.
Deliver envelopes to their recipient alone, inside the SDK.

## Ordered slices

1. Decision record: the delivery record, who may hold it and its bounds. An
   envelope is addressed to a recipient by a value only the recipient and the
   sender can link to its node key.
2. Outbox: `share` and rotation save pending envelopes durably with the
   intent, before any network call.
3. Direct delivery: a session with the recipient hands over its pending
   envelopes after admission; the recipient verifies and installs as today
   and acknowledges; the sender then drops them.
4. Mailbox: a sender deposits an envelope with a peer that may hold it. The
   mailbox learns neither the partition nor the secret and serves an envelope
   only to the session that proves the recipient's key. Deposits are bounded
   per recipient and expire.
5. A delivery to a recipient revoked meanwhile is dropped, not served.
6. The application surface: `share` no longer returns envelopes to carry;
   status reports pending and delivered recipients to the sharer alone.

## Completion gate

- [Acceptance 8](scope-mesh.md#acceptance) passes for a share and for the
  envelopes of a rotation, the recipient being offline at the time and
  meeting only the custody peer afterwards.
- A third member's replica and sessions contain nothing that links the
  recipient to the partition.
- A mailbox serves an envelope only to its recipient; a full mailbox refuses
  with a retryable error and the sender keeps the envelope.
- An envelope delivered twice installs once.
- A crash between `share` and delivery, and between delivery and
  acknowledgment, loses no envelope.
- A recipient revoked before delivery receives nothing.
- [Partitioned scopes](../runtime/partitioned-scopes.md) describes it.

## Not in this phase

Removing the `keys` table from the shopping example
([phase 17](17-shopping-without-a-roster.md)).
