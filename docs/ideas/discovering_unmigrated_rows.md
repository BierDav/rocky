# Discovering rows a migration has not reached

Status: idea; not planned.

A [migration](../runtime/generations.md#migrations) runs once, on the issuer
that signs its generation, over the rows that issuer holds. Rows it did not
hold, and rows devices on an older application write afterwards, never pass
through the migration: they read the field's default. For many fields the
default is enough. When it is not, such as a sort position or a value derived
from other columns, those rows stay wrong until someone edits them.

## Idea

Trusted nodes find such rows and migrate them later, as ordinary writes:

- **Detection.** A row needs the migration when it was written under a
  generation older than the migration's, or created after the migration
  committed, and its migrated fields still read their default. A trusted
  node would find them from its stored rows and their stamps, also as frames
  arrive; whether rows need to record the generation of their writes for
  this is open.
- **Row-scoped steps.** The migration declares, per step, whether it may run
  again for chosen rows: an `UPDATE` with a `WHERE` the node narrows to those
  rows, or an `insert` keyed by name. Steps that only make sense once are not
  repeated.
- **Writing.** The node runs the step for those rows and publishes the result
  like any write. A field someone set after the migration is newer and is
  never overwritten: only fields still at their default are filled.
- **Who runs it.** Any trusted node that reads and writes the row's partition.
  Concurrent fills write the same values or meet by latest wins; a claim in
  the [coordinated log](../runtime/coordinated-log.md) could avoid duplicate
  work.

## Open questions

- How a step declares that it is safe to run per row, and how Rocky checks it.
- Whether a default the application set on purpose can be told apart from
  one no migration reached.
- How long trusted nodes keep looking: until every older generation's frames
  are finalized, or for good.
