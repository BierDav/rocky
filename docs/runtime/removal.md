# Removal

A [generation](generations.md) may remove mergeable tables and fields, and a
later one may add a removed name again ([decision 0067](../decisions/0067-names-are-identities.md)
keeps its meaning and type). Every device that adopts it drops the same
things the same way. [Decision 0068](../decisions/0068-adopting-newer-generations.md)
records why.

## Classification

`rocky_core::classify` reports removed tables and `table.field`s in
`SchemaChange::removed()`. A rename is a removal and an addition; values are
not carried over. A removed key or partition column changes the table's key
and is refused: remove the table instead. Indexes, foreign keys, views,
partition groups and `granted_like` entries naming a removed item go with it,
since the new schema must validate on its own. Command-controlled tables stay
unchangeable. The replica's `extends` keeps every method, operation and
domain, and every table and field the new manifest still names, unchanged;
the transitions between two held manifests must remove whatever the newer one
lacks, and one step must name and remove exactly what they differ in.

## Names of a lineage

The replica derives from the signed transitions of its recorded lineage,
which list the table and field ids each step named and removed; it needs no
earlier manifest, since those are a [cache](generations.md#manifests-are-a-cache):

- **retired** tables and fields, removed by a transition and not named
  again since;
- **re-added** ones, with the baseline of the generation that named them
  again: the signing time of its transition in microseconds, the earliest
  one learned. A re-added table's fields share its baseline.

Every write is read through them: a retired field or table is ignored, and a
re-added one is ignored when stamped before its baseline. A re-added table's
row created before the baseline is gone, and so is every later write to it.

Stamps are hybrid time: the larger of the logical step past every value a
write saw and the wall clock in microseconds, so they compare with a
baseline. A device whose clock runs far ahead stamps later than it wrote;
every device still applies the same rule to the same stamps.

## Adopting a removing generation

In the activation transaction the replica:

- holds and voids incompatible never-offered local bundles, scheduling their
  kept scopes for replay ([held changes](held-changes.md));
- deletes a removed table's stored rows, row keys, subscription and
  retention policy; its signed history and checkpoint stay, so a generation
  that adds it again can pull them again;
- strips retired fields, and re-added ones stamped before their baseline,
  from every stored row and from the verified checkpoint base, which it
  seals again under the generation that removed them;
- queues every scope with a new or earlier baseline to pull again.

The [projection plan](generations.md#plan-backfill-and-switch) then
recreates the tables, indexes and constraints of the new schema from the
stored rows. Stored rows stay a replay of the checkpoint base and the frames
after it, which the open-time audit checks. File identity
`rocky.encrypted-replica.v25` (including [held changes](held-changes.md) and
[coordinated writes](coordinated-writes.md)).

## Frames and checkpoint pages

A frame's writes are authorized as signed; the replica then applies what the
current names keep. Relayed frames stay as signed. A frame of a removed
table is refused and is admitted again when the table returns. A checkpoint
of an earlier generation verifies as signed; when its import finishes, its
rows and base are stripped like stored rows.

## Pulling again

After the switch, and after a device learns an earlier baseline
(`EncryptedReplica::learn_baseline`) or an older generation, each queued
scope is rebuilt from its checkpoint base and the retained frames after it,
one transaction per scope. The queue (`secure_repull`) survives a crash.
Writes covered by the base and pruned are not recovered; such a copy differs
from the finalized cut and imports the
[checkpoint at a seal](lazy-compaction.md#checkpoints-at-seals).

## The SDK

Every profile is interpreted through `Domain::effective`, which drops
removed tables and columns from reader, writer, custody and grant profiles
and from partition grants. A removed table's reader keys stay with the
identity, so a generation that adds it again opens its earlier frames; only
current tables' keys reach the replica.

An application whose schema matches an earlier generation is
[behind](adopting-generations.md#applications-behind-and-ahead), unless it
declares a later version than the installed schema: that version may add a
removed name back. An application older than its database keeps working on
what is unchanged; its statements on what was removed fail with ordinary
SQLite errors, under Room too ([Room integration](room-integration.md#older-applications-after-removal)).

The first reader key of a table a generation adds derives from the
registry's current key, `rocky.sdk.generation-reader.v1`, the domain and the
table name, so issuers that sign the same schema apart hold the same key. A
revoked issuer, which receives no later registry key, cannot derive it.

## Limits

SDK signing requires [application confirmation](generations.md#confirmed-removals);
held changes are implemented in [phase 4](../features_09/04-held-changes.md). A remaining
table's reader scope must stay as it was: a removal that would split a
reader scope another table shares is refused by the projection plan.

## Evidence

- `rocky-core` `generation::tests::removed_tables_and_fields_are_replicated_changes`.
- `rocky-secure` `tests::generations::a_generation_removes_a_field_and_a_table_on_every_replica_that_adopts_it`
  and `a_readded_field_ignores_earlier_writes_and_pulls_again_the_later_ones`,
  which also learns an earlier baseline and imports a finalized checkpoint.
- `rocky-sdk` `tests/removal.rs`: `a_generation_removes_a_field_its_unique_index_and_a_referenced_table`
  and `devices_that_explicitly_open_a_readded_field_agree_on_it`.
- `rocky-sdk` `tests/generations.rs` `issuers_that_sign_one_schema_apart_end_on_one_generation`
  (derived reader keys).
- Kotlin `RoomGenerationsTest`.
