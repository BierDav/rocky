# 20. Seal qualification

Status: proposed; not started.
Owner: `examples/shoppinglist-app` and its qualification harness. Depends on
[phases 12](12-qualification.md), [18](18-drafts-and-sandboxes.md) and
[19](19-recovery-as-a-bookmark-move.md). Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks) and
[qualification](12-qualification.md).

[Phase 12](12-qualification.md) qualifies removal with real app versions.
This phase repeats that scenario on seals and adds compaction, kept seals,
drafts, splits and rollback with the same devices.

## Settled

- **Devices** as in phase 12: a trusted desktop on app version 9, a member
  desktop on 8 and an Android phone on 7, all following `main`.
- **Compaction.** The shopping app calls `seal()` from its own policy; the
  phone is offline across one seal and returns.
- **Kept seals.** The trusted desktop keeps every seal and shows a list as
  it was two seals ago through a read-only view.
- **Schema 9 by draft.** The trusted desktop drafts schema 9 with a
  migration, tests it and publishes. The offline member's writes come back
  as held changes and its app re-authors them.
- **Split and rollback.** Two issuers publish different schemas at once;
  the household ends on one. The trusted desktop then moves `main` back one
  seal, and every device follows.

## Ordered slices

1. Seal policy in the shopping app; compaction and kept seals.
2. Schema 9 through a draft; held changes on the member.
3. Concurrent publication and rollback.
4. Device qualification on desktop and Android.

## Completion gate

- Every device ends with the same lists after each scenario.
- After compaction, no device's file holds tombstones of sealed
  generations it compacted and does not keep.
- `./examples/shoppinglist-app/check` passes; seal adoption, compaction and
  view timings are recorded with the qualification evidence.
