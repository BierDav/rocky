# 0069: Held changes and void records

Status: accepted; implemented by [Phase 4](../features_09/04-held-changes.md).
Date: 2026-10-14.

## Context

[Removal](../runtime/removal.md) applies old frames field by field. But a
device adopting a removal may still hold local transactions nobody was
offered. Sending only their kept fields would lose the transaction's intent;
deleting their frames would strand later positions in the author logs.

## Decision

- Record each locally authored frame as never offered, with its original
  writes and whole-bundle identity. Durably remove that mark before returning
  a transfer/range page, a checkpoint covering it or a recovery package.
  Failed delivery still counts. Any offered member protects the whole bundle.
- In generation activation, hold whole never-offered bundles touching a
  removed table or assigning a removed field a non-default value. Hold their
  never-offered causal dependants too. Save their named row images, remove
  their frames and outbox entries, and schedule kept scopes for projection
  rebuild from admitted history. The resumable generation switch finishes
  before the SDK opens the database or admits newer frames.
- Cancel each held position with `rocky.void.v1`, signed by its log author
  over up to 64 original signed headers of one scope and author. Table
  pseudonyms sign pending records when their keys are registered after open.
  Verify the record and every header; require local whole-scope custody or
  read authority. Refuse a record covering a stored frame, a conflicting
  position or a position the log already passed. Duplicate records are safe.
- Voids consume log positions and travel in authenticated pages and pushes.
  Dependencies through them wait for the preceding admitted history. An
  incoming frame at a voided position is refused as a fork. No receiver can
  prove that a malicious author never offered a frame elsewhere: admission
  preserves its first history, as for other author forks.
- Expose held rows and their original values, plus each written kept field's
  current value, time and author. Applications map them to the current schema
  and commit ordinary fresh writes, resolving the held ID atomically with
  that commit, or explicitly drop the change. A re-authoring handle binds
  both its writes and the held ID; errors roll both back. Empty re-authoring
  is refused; dropping is the explicit alternative.

## Consequences

The encrypted file identity is `rocky.encrypted-replica.v17`; the live-link
handshake is `rocky.link.v2`. Other versions are refused, with no migration
or dual encoding. All external calls remain outside database transactions.

[Held changes](../runtime/held-changes.md) specifies Rust, JSON and Kotlin
APIs, limitations and [qualification evidence](../features_09/04-held-changes-evidence.md).
