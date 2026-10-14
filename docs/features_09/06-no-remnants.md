# 6. No remnants

Status: implemented (2026-10-14) except end-to-end proof that a frame of a
dropped generation applies after the fetch; see [evidence](#evidence).
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
- **Required fields are additive.** A field that is required with a default,
  new or once nullable, is an additive change: rows from older writers read
  the default. A NULL that a frame of an older generation writes to a field
  made required reads as its default, on every device; the switch rewrites
  stored NULLs the same way.
  `migrations` stay and run once on the first issuer as ordinary signed
  writes, of any size after [phase 7](07-migrations-at-any-size.md);
  nobody's work is dropped.
- **Shopping schema 7** becomes an additive generation; its migration is
  unchanged.
- **Manifests are a cache.** A device drops a manifest, other than its
  installed generation's, once no retained frame, checkpoint or credential
  names it; [phase 2](02-adopting-newer-generations.md)
  fetches it again from whoever serves such a frame. Retained history follows
  the existing horizon finalization, independent of schema versions.
- **Removed names** leave nothing behind.

## Ordered slices

1. Remove the cut code and its tests; required-with-default becomes
   additive; shopping schema 7 and `SchemaUpgradeTest` follow.
2. Dropping unreferenced manifests.
3. Guides: generations, decision 0066 status, shopping schema 7.

## Completion gate

- No code path signs a transition whose horizon is its new generation.
- A v6 device keeps syncing with v7 devices; its new items read the default
  position.
- A v6 frame that writes NULL to a field v7 made required reads as the
  default on every v7 device.
- Once its last frame is finalized, a version's manifest is gone from every
  device; a frame of that version arriving later fetches it again and applies.
  *Partly verified:* the drop on every device and the fetch are shown end to
  end; that the fetched frame applies is shown only in `rocky-secure`. In the
  SDK test the frame stays hidden after the fetch because its writer missed a
  finalized horizon, with dropping disabled as well; see [evidence](#evidence).

## Evidence

Decision record [0070](../decisions/0070-confirmed-rollouts-and-the-end-of-cuts.md)
and the guides [generations](../runtime/generations.md#required-fields)
(required fields, manifests are a cache) and
[shopping schema 7](../runtime/shopping-schema-7.md).

- Slice 1. `SchemaChange::Breaking`, the transition's cut flag, the replica's
  stored horizon, per-cut pseudonym roles, the cut's bootstrap intent and the
  refusal of a migration that leaves NULL are gone; no transition can carry a
  horizon. `rocky-core` `required_fields_with_defaults_are_additive` and
  `transitions_step_forward_and_round_trip`.
- A v6 device keeps syncing with v7 devices and its new items read the
  default: `rocky-sdk` `tests/generations.rs`
  `a_required_field_migrates_on_the_issuer_and_older_devices_keep_syncing`
  (an application at version 1 adopts the generation, its new item reads
  `''`, a member's unsynced item of generation 1 arrives) and the shopping
  example's `SchemaUpgradeTest` (a version 6 member keeps syncing on its
  version 7 database; its offline item arrives and a new one reads the
  default position). Running it first needed a fix to
  [phase 2](02-adopting-newer-generations.md)'s discovery: a member whose
  link to the issuer failed on the issuer's newer credentials never adopted
  and backed off; it now adopts, or learns an older generation, from that
  peer and is no longer backed off.
- A NULL an older generation writes reads the default on every device, and the
  switch rewrites stored NULLs:
  `a_null_an_older_generation_writes_to_a_required_field_reads_its_default`
  and `rocky-core` `a_required_field_rows_lack_reads_its_default`.
- Slice 2. Transitions (`rocky.transition.v3`) carry the field ids they add;
  the SDK lineage keeps every transition and a manifest only while the
  replica holds it. After a horizon finalizes, both devices of
  `a_manifest_nothing_retained_names_is_dropped_and_fetched_again` drop the
  generation 1 manifest from replica and saved lineage; when a device that
  stayed away returns with a generation 1 frame, the receiving device fetches
  the manifest again (`generation_learned`) and holds it in its lineage.
  `rocky-secure` `an_unreferenced_manifest_is_dropped_and_learned_again_from_a_frame`
  drops two manifests, keeps the baseline, refuses a frame of the dropped
  generation and a forged manifest, learns the writer's manifest through the
  kept transitions and applies the frame, after which the manifest stays.

Not shown end to end: the returning frame in the SDK test stays hidden after
the fetch. The same happens with dropping disabled: the device missed a
finalized horizon, whose existing rules hide its write. A device that
acknowledged the horizon at generation 1 cannot be used instead, since a peer
upgraded after a finalized horizon refuses that horizon's statement as "not
current" and closes the link, a limit of [phase 2](02-adopting-newer-generations.md)'s
horizon handling. Chains peers last authenticated with do not keep a manifest
(decision 0070); otherwise any device that was ever away would pin its
generation's manifest everywhere.

Checks on 2026-10-14: `./scripts/lint` and `sdks/kotlin/gradlew -p sdks/kotlin check`
passed. `./scripts/check` ran every test green (173 unit, 995 integration) but
failed its timing budgets under heavy machine load; the tests over budget take
about 1 s alone. `./examples/shoppinglist-app/check`, run on a JVM/Android
project (`scripts/ci/shopping-project`) against this change's SDK, passed
`SchemaUpgradeTest` but failed intermittently in `HouseholdTest`: a household
reset and a Room background failure close the node while `createList` runs.
The Phase 2 SDK passed the reset test 6 times of 6, this change 3 of 4. That
looks like a regression and has not been explained.
