# 1. Names as identities

Status: implemented (2026-10-14); see [evidence](#evidence).
Owner: `rocky-core` definitions, `LogicalSchema` and `classify`;
`sdks/kotlin/rocky-core` builder and Room overlay. Follow the
[execution contract](README.md#agent-execution-contract). Takes decision
record [0067](../decisions/0067-names-are-identities.md).

## Inputs and objective

Read [logical schema](../runtime/logical-schema.md),
[schema definitions](../runtime/schema-definitions.md) and
[Room integration](../runtime/room-integration.md).

`LogicalSchema::field_id` hashed `table.logical` and `field.logical`, an
optional identity separate from the SQL name. A separate identity is a
translation layer every device, grant, key and wire name would have to
follow. After this phase there is none: a table or field's name is its
identity, forever.

## Settled

- **The name is the identity.** Ids derive from the publisher, application
  and name. Definitions, descriptors, the Kotlin builder and the Room overlay
  have no `logical` entry; definitions move to format 6 and descriptors to
  format 5, and other formats are refused.
- **A name keeps its meaning.** A field removed and later added again under
  the same name is the same field with the same meaning and type; keeping
  that true is the developer's responsibility. To change what a field means,
  give it a new name. An application that must keep old versions from
  misreading a name enforces a minimum app version itself.
- **Renaming** is removing the old name and adding a new one, so it is a
  [removal](03-removal.md); until then `classify` refuses it.
- **One name, one type.** `classify` refuses a changed type, collation or
  key role for a name.

## Ordered slices

1. Decision record 0067; `logical` removed from definitions, descriptors
   and `classify`.
2. Builder and Room overlay.
3. Guides: logical schema, schema definitions, Room integration.

## Completion gate

- No definition or descriptor carries an identity apart from its names; a
  `logical` entry is refused.
- Renaming a table or field changes the manifest and is classified as a
  removal.
- A type, collation or key-role change on a name is refused.

## Evidence

- `rocky-core` `generation::tests::names_are_identities_and_keep_one_type`:
  a rename changes the manifest and is `RemovedField`; type and collation
  changes are `ChangedField`, a new key role is `ChangedKey`.
- `rocky-sdk` `tests/generations.rs`
  `a_name_keeps_its_type_and_a_rename_is_another_field`: an SDK database
  refuses to open with a retyped or renamed field and reopens unchanged.
- `rocky-sdk` `tests/typed.rs` `renamed_tables_and_columns_are_other_items`:
  a renamed table or column changes the replicated fingerprint and manifest.
- `rocky-core` definition tests refuse formats other than 6 and descriptor
  formats other than 5.
- Kotlin: `:qualification:schema:check` keeps the hand and Room shopping
  declarations byte-equal without an overlay identity.

On 2026-10-14 `./scripts/lint`, `./scripts/check` (173 unit, 969
integration tests), `sdks/kotlin/gradlew -p sdks/kotlin check` and
`./examples/shoppinglist-app/check` passed.
