# 1. Stable identities

Status: proposed; not started.
Owner: `rocky-core` definitions, `LogicalSchema` identities and `classify`;
`sdks/kotlin/rocky-core` builder and Room overlay. Follow the
[execution contract](README.md#agent-execution-contract). Takes decision record 0067.

## Inputs and objective

Read [logical schema](../runtime/logical-schema.md),
[schema definitions](../runtime/schema-definitions.md) and
[Room integration](../runtime/room-integration.md).

`LogicalSchema::field_id` hashes `table.logical` and `field.logical`.
`logical` already separates identity from the SQL name, but the Kotlin
builder cannot set it on columns. After this phase identities are explicit, safe to
rename around, and need no tooling in any language.

## Settled

- **A string, defaulting to the name.** `logical` becomes `identity`: any
  string unique in its table, the name when omitted. Most declarations never
  write one. Ids still derive from the publisher, application and identity.
- **Renaming** sets the new name and keeps the old identity, for example
  `text("label", identity = "title")`. It is a local change: no transition.
- **A new field** with a removed field's name needs a new identity, such as
  `category2`. Generating random identities may come as tooling later.
- **Re-adding an identity** a generation removed is allowed and means the
  same field again; its meaning and type are the developer's responsibility.
  How devices converge on it is an
  [open question](README.md#open-questions).
- **One identity, one type.** `classify` refuses a changed type, collation or
  key role for an identity.
- **Kotlin.** `identity` on columns and tables in the builder and the Room
  overlay; `RequiredSchema` keeps the hand and Room declarations equal.

## Ordered slices

1. Decision record 0067; `identity` in definitions and descriptors and
   `classify`.
2. Builder and Room overlay.
3. Guides: logical schema, schema definitions, Room integration.

## Completion gate

- Renaming a table or field on one device is a local change: the manifest
  hash is unchanged, and a device that did not rename syncs the same values.
- A type change on an identity is refused.
