# 15. Horizon

Status: complete; all completion bullets and development gates qualified
(2026-10-08). Owner: `rocky-secure` checkpoints, maintenance and
apply; `rocky-credentials` for the signed statement; `rocky-sdk`. Depends on
[phase 3](03-bootstrapping-from-checkpoints.md),
[phase 7](07-row-identity.md),
[phase 12](12-hidden-rows-unique-indexes-and-foreign-keys.md) and
[phase 14](14-existence-log-and-puppets.md). Wave 4. On the critical path.
This is slice B of section 6; its checkpoint change lands after
[phase 9](09-strong-history-behind-the-checkpoint.md)'s.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [slice B](agreed-scope.md#slice-b-dropping-tombstones-and-hidden-rows-new-format)
and [decided](agreed-scope.md#decided) of the scope,
[encrypted checkpoints](../runtime/encrypted-checkpoints.md) and the trusted
path's pruning in [decision 0010](../decisions/0010-checkpoints-and-pruning.md).

Tombstones are never dropped and hidden rows never become final. Hidden rows
can return, so every device must finalize them at the same cut: the domain's
issuer signs a horizon statement and the others follow. A device that has not
acknowledged is declared behind after the shorter of its credential's
remaining validity and 30 days, and then rebootstraps through phase 3.

## Ordered slices

1. Decision record: the horizon statement, the cut for a scope and the
   existence logs its rows reference, the checkpoint change, versioning and
   refusal.
2. The statement and its signer; acknowledgment tracking; the behind rule.
3. Finalization at the cut: tombstones dropped, hidden rows and orphans
   finalized identically on every holder, `SET NULL` stored as NULL, every
   holder syncing the referenced existence logs to the cut first. The highest
   `id` sequence per author and partition survives.
4. After the horizon: an edit or delete of a row that does not exist is
   dropped definitely, including an edit whose row creation was pruned; an
   insert that conflicts with an existing row is dropped. Dropping a local
   operation never ends its author's log, on both profiles.
5. A device whose frontier is behind the horizon must rebootstrap and cannot
   resurrect a dropped row.

## Completion gate

- After a horizon, every replica holds the same rows and the same digest,
  including devices that cannot read the parents of their orphans.
- A device offline past the behind rule rejoins by rebootstrap; a resurrected
  tombstone never appears.
- A pending edit whose creation was pruned is dropped and the author's log
  continues.
- Storage of a long-running scope plateaus; measured and recorded.
- [Encrypted checkpoints](../runtime/encrypted-checkpoints.md) describes the
  horizon.

## Implementation verdict

The remaining core is implemented: atomic exact-cut finalization, signed paged
existence bases, format-6 finalized checkpoints/file fencing, definite drops
with continuing author logs, and SDK/native/Kotlin declarations and enforced
phase-3 rebootstrap. Finalized base keys reject stale conflicts; concurrent
post-cut inserts into vacant keys retain phase-12 deterministic winners.

[Decision 0036](../decisions/0036-mergeable-horizon.md) specifies the upgrade,
refusal, causal key-reuse and retention policy. [Measured evidence](../runtime/horizon-evidence.md)
maps every completion bullet to executable regressions and final gates. The
64-cycle two-holder churn probe retains zero tombstones/existence records and
plateaus at 170 pages / 696,320 database bytes at cycles 16 and 64. Applications
invoke issuer maintenance periodically and retain the ordinary durable-copy gates.

Final gates exit 0: `./scripts/check` (606 workspace and 117 no-default SDK tests;
153 secure tests including the 4,100-command soak), credentials (23), horizon
(21), existence (20), native and Kotlin SDK/schema checks, and Kotlin schema (4),
Room (12) and SQLite (5). No completion blocker remains.
