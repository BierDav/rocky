# Removal

A [schema change](generations.md) may remove mergeable tables and fields,
and a later one may add a removed name again ([decision 0067](../decisions/0067-names-are-identities.md)
keeps its meaning and type). Every device that follows the bookmark across
it drops the same things the same way.

## Classification

`rocky_core::classify` reports removed tables and `table.field`s in
`SchemaChange::removed()`. A rename is a removal and an addition; values are
not carried over. A removed key or partition column changes the table's key
and is refused: remove the table instead. Indexes, foreign keys, views,
partition groups and `granted_like` entries naming a removed item go with
it, since the new schema must validate on its own.

## The path filter

A write reaches the live tip through every tip on the canonical path from
the tip it was written on: `PathFields` is the intersection of the table and
field ids of each tip's schema in the [catalog](adopting-generations.md#the-schema-catalog).
A frame written on A reaches C through B; if B removed `note`, the value is
stripped, so a field removed and added again never receives a value written
before its removal. Frames written on C carry `note`. No clock, baseline or
pull-again is involved.

The filter applies wherever stored state meets another tip: frames applied
live, the open-time replay, forward switches, and checkpoints. A
replay base at seal X keeps X's own fields; readers filter it with the path
from X to their tip, and its signed bytes are never rewritten. A frame whose
fields are not a subset of its source tip's schema is refused.

A backward or sideways switch rebuilds the target's own state. Its starting
seal or base is filtered only along its path to the target, and each replayed
frame along its own path to the target. Following `main` after `dev` removed
`note` therefore restores `main.note` from `main`'s history. There is no
intersection across the two branches. A kept seal is rewound directly only
when its path to the target removes none of that seal's fields; otherwise the
replica uses a filtered base or frame replay. Read-only kept-seal views stay exact.

## Following a removing schema

In the switch transaction the replica:

- holds and voids incompatible never-offered local bundles visible at the
  target, checking each frame's path to that target
  ([held changes](held-changes.md));
- deletes a removed table's stored rows and strips removed fields from every
  stored row through the path filter;
- recreates the tables, indexes and constraints of the new schema from the
  stored rows through a projection plan.

Going A→C directly and walking A→B→C end the same.

Changing which bookmark a device follows holds and voids no source-line
writes merely for leaving that line: unsent writes stay there until it returns.
This cause persists across a pending schema switch and reopening. When the
followed bookmark itself moves backwards or sideways, only frames visible at
the source and excluded by the target become held. A common ancestor's late
frame that reaches the target through `continue` seals remains applicable.

## The SDK

Every profile is interpreted through `Domain::effective`, which drops
removed tables and columns from reader, writer, custody and grant profiles
and from partition grants. A removed table's reader keys stay with the
identity, so a schema that adds it again opens its earlier frames.

The first reader key of a table a schema adds derives from the registry's
current key, `rocky.sdk.generation-reader.v1`, the domain and the table
name, so issuers that publish the same schema apart hold the same key.

## Limits

Publishing requires [application confirmation](generations.md#confirmed-removals).
A remaining table's reader scope must stay as it was. A birth one branch's
filter withheld is restored on another target only through the checks in
[admission evidence](admission-evidence.md#receipts).

## Evidence

- `rocky-core` `generation::tests::removed_tables_and_fields_are_replicated_changes`.
- `rocky-secure` `tests::generations::switches`:
  `a_schema_removes_a_field_and_a_table_on_every_replica_that_follows_it` and
  `a_readded_field_never_receives_a_value_written_before_its_removal`, which
  also imports a base at A's seal into C.
- `rocky-secure` `tests::kept::a_seal_before_a_removal_still_shows_the_removed_field_and_table`.
- `rocky-secure` `tests::generations::sideways`: target restoration, unsent
  source-line writes, branch isolation, late common-ancestor frames, rollback
  and identical-manifest removal/re-add through kept, base and replay paths.
- `rocky-sdk` `tests/removal.rs`:
  `a_generation_removes_a_field_its_unique_index_and_a_referenced_table` and
  `a_readded_field_never_receives_a_value_written_before_its_removal`.
