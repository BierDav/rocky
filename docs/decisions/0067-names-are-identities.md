# 0067: Names are table and field identities

Status: accepted and implemented.
Date: 2026-10-14.

## Context

Table and field ids hashed an optional `logical` identity that could differ
from the SQL name, so a rename could keep the id. Making that useful meant
translating between identities and names wherever a device names a table:
grants, reader keys, author pseudonyms, enrollment state and wire messages.
Schemas that evolve over decades need removal and re-adding
([features_09](../features_09/README.md)), not renames that keep ids.

## Decision

- A table or field's name is its identity, forever. Ids derive from the
  publisher, application and name; there is no separate identity entry.
- A name keeps its meaning and type. A field removed and added again under
  the same name is the same field; the developer keeps that true, and gives
  a field whose meaning changes a new name. An application that must keep
  old versions from misreading a name enforces a minimum app version.
- Renaming is a removal and an addition. `classify` refuses a changed type,
  collation or key role for a name.

## Consequences

Definitions move to format 6 and descriptors to format 5; a `logical`
entry and other formats are refused. The Kotlin builder and Room overlay
lose `logical`. Renaming waits for [removal](../features_09/03-removal.md),
which drops the old field on every device. See
[logical schema](../runtime/logical-schema.md#descriptor-and-fingerprints).
