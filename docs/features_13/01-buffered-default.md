# 1. Buffered default

Status: planned. Owner: `rocky-sdk` (`Database::transaction`). Depends
on nothing. Effort: one to two days. Carried over from features_12
phase 4, slice 1. Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

Stop the Rust SDK from paying a device flush on every interactive
commit. In features_12 phase 1, desktop L0 measured Flushed p50 77.0 ms
against Buffered 3.5 ms. This is the largest single idle-latency win and
needs nothing else.

## Settled

- `Database::transaction` opens its connection with
  `Durability::Buffered`, matching the Kotlin binding.
- Its doc comment states the guarantee: the write is visible to
  observers and readers on return, survives an application crash
  (WAL at `synchronous=NORMAL`), not power loss, and becomes durable and
  publishable at the background flush (`settle_commits`).
- Callers that need a flushed commit open a connection with
  `Durability::Flushed`. No new SDK API is added.
- Tests that relied on the old default switch to `Flushed` explicitly.
  They don't wait on timing.

## Ordered slices

1. Change the default and the doc comment; adjust the tests that relied
   on it.
2. Rerun the L0 probe on desktop, both durability modes, paired with the
   phase-start commit.

## Completion gate

- Desktop L0 interactive write p95 ≤ 3 ms, recorded with its profile.
- Observer notification p95 ≤ 5 ms after `commit()` returns.
- `./scripts/check`, the Kotlin check and the shopping app check pass.

## Not in scope

Statement cuts (they become intent-path budgets in
[phase 4](04-intent-replay.md)) and lock behavior ([phase 2](02-interactive-first.md)).
