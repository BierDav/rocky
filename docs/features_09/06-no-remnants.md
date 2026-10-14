# 6. No remnants

Status: proposed; not started.
Owner: `rocky-core` generations; `rocky-secure` horizon, retention and
pruning; `rocky-sdk` `generation.rs` and `database.rs`; the shopping example.
Depends on [phase 2](02-adopting-newer-generations.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision record 0070.

## Inputs and objective

Read [generations](../runtime/generations.md),
[decision 0066](../decisions/0066-breaking-generations.md) and
[mergeable horizon](../runtime/mergeable-horizon.md).

A schema that evolves for decades must not accumulate state. Two things
would: the cut mechanism, which no longer has a purpose, and the manifests of
every version that ever wrote. After this phase neither remains.

## Settled

- **Cuts are removed.** `SchemaChange::Breaking`, a horizon equal to the new
  generation, the forced rebootstrap, per-cut pseudonym keys and re-sealing
  for cuts are deleted. Decision 0066 is superseded in those parts.
- **Required fields are additive.** A field that is required with a default
  is an additive change: rows from older writers read the default.
  `migrations` stay and run once on the first issuer as ordinary signed
  writes, of any size after [phase 7](07-migrations-at-any-size.md);
  nobody's work is dropped.
- **Shopping schema 7** becomes an additive generation; its migration is
  unchanged.
- **Manifests are a cache.** A device drops a manifest once no retained
  frame, checkpoint or credential names it; [phase 2](02-adopting-newer-generations.md)
  fetches it again from whoever serves such a frame. Retained history follows
  the existing horizon finalization, independent of schema versions.
- **Removed identities** leave nothing behind.

## Ordered slices

1. Remove the cut code and its tests; required-with-default becomes
   additive; shopping schema 7 and `SchemaUpgradeTest` follow.
2. Dropping unreferenced manifests.
3. Guides: generations, decision 0066 status, shopping schema 7.

## Completion gate

- No code path signs a transition whose horizon is its new generation.
- A v6 device keeps syncing with v7 devices; its new items read the default
  position.
- Once its last frame is finalized, a version's manifest is gone from every
  device; a frame of that version arriving later fetches it again and applies.
