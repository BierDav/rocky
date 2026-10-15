# 20. Seal qualification

Status: proposed; not started.
Owner: `examples/shoppinglist-app` and its qualification harness. Depends on
[phases 18](18-drafts-and-sandboxes.md) and
[19](19-recovery-as-a-bookmark-move.md). Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[shopping schema 7](../runtime/shopping-schema-7.md) and
[shopping qualification](../runtime/shopping-qualification.md).

This phase qualifies the shopping app on seals with real app versions on
real devices: compaction, kept seals, branches, drafts, splits and
rollback.

## Settled

- **Devices.** A trusted desktop on app version 8, a member desktop on 7
  and an Android phone on 7, all following `main`, the app's default
  bookmark. The app has a bookmark selector.
- **Compaction.** The shopping app calls `seal()` from its own policy; the
  phone is offline across one seal and returns.
- **Kept seals.** The trusted desktop keeps every seal and shows a list as
  it was two seals ago through a read-only view.
- **A dev branch.** The trusted desktop opens a branch tip on `main`'s
  latest seal and points `dev` at it. The member desktop selects `dev`;
  both write, and a coordinated write runs through `dev`'s log. The phone
  sees nothing. The member switches back to `main` and drops `dev`.
- **Schema 8 by draft.** The trusted desktop drafts schema 8 with a
  migration, tests it and publishes. The offline member's writes come back
  as held changes and its app re-authors them.
- **Split and rollback.** Two issuers publish different schemas at once;
  the household ends on one. The trusted desktop then moves `main` back one
  seal, and every device follows.

## Ordered slices

1. Seal policy in the shopping app; compaction and kept seals.
2. The bookmark selector; a dev branch with switching and local drop.
3. Schema 8 through a draft; held changes on the member.
4. Concurrent publication and rollback.
5. Device qualification on desktop and Android.

## Completion gate

- Every device ends with the same lists after each scenario.
- After compaction, no device's file holds tombstones of sealed tips it
  compacted and does not keep.
- While `dev` is written, the phone receives none of its frames and no
  `dev` entry reaches `main`'s coordinated log.
- `./examples/shoppinglist-app/check` passes; seal adoption, compaction and
  view timings are recorded with the qualification evidence.
