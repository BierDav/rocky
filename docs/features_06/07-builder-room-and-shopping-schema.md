# 7. Builder, Room import and the shopping schema

Status: not started.
Owner: the Kotlin builder, Room import and schema qualification; the shopping
example. Depends on phases [4](04-partition-aware-constraints.md) and
[5](05-partitioned-foreign-keys.md). Wave 3.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the schema that must install](scope-partitioned-constraints.md#the-schema-that-must-install)
and requirement [6](scope-partitioned-constraints.md#6-kotlin-builder-and-room-import)
of the scope. The Kotlin `SchemaBuilder` refuses a conflict policy or
reference on a partitioned table before installation.

## Ordered slices

1. Lift the builder's refusal for the shapes of phases 4 and 5 only. Keep a
   precise message for each shape that stays refused.
2. Multi-column references in the builder, and from Room's `@ForeignKey`.
3. Room import: a Room export with partitioned entities, groups in the
   overlay and these constraints imports to the same definition as the
   hand-written schema.
4. Shopping example: `lists`, `items` and `categories` in one group, with the
   unique title index, the cascading list reference and the nullable
   `SET NULL` category reference; DAOs and the Compose app use categories.
5. `./kotlin run -m requiredSchema` in `examples/shoppinglist-app` passes
   and joins the checked scripts.

## Completion gate

- `./kotlin run -m requiredSchema` passes; the definition
  installs natively and from Room with an identical descriptor.
- The packaged Kotlin consumer builds the schema with no Rust source.
- Each remaining refusal has a Kotlin check with its message.
- The Compose app creates a list, shares it with one call and assigns and
  deletes a category.
