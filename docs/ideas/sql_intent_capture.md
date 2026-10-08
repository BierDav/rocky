# Capturing the intent of SQL writes

Status: idea, postponed. Not designed and not scheduled.

## Current limitation

A commit publishes the rows it changed, by comparing each row before and after
the transaction. What the statement meant is lost. `SET quantity = quantity + 1`
publishes the number the author computed, so two devices incrementing
concurrently keep one of the two results. See
[SQL sessions](../runtime/sql-sessions.md#what-a-commit-publishes).

Until this is built, an application that needs a merged counter or set uses a
strong command, or models it as rows (one row per increment or per member).

## Idea

Every statement is already parsed before the engine sees it
([design rationale](../runtime/queries-and-observation.md#design-rationale)). Use
that syntax tree to recognise a small set of intents on columns that declare a
merge type, and publish the intent instead of the value:

- A `counter` column with `SET n = n + ?` or `n - ?` publishes the difference,
  so concurrent increments add up. The same statement keeps working through
  Room's `@Query`.
- Other merge types, such as a set with add and remove, would follow the same
  pattern, one at a time and only when an application needs it.

Rows stay the source of truth. The syntax tree only annotates columns of a
declared type; everything else keeps merging as last writer wins.

A separate query language in the style of
[Ditto's DQL](https://docs.ditto.live/dql/update#increment-by) was considered
and not preferred: the limitation comes from what is recorded, not from the
syntax, and a second language would not work through ORMs.

## Why not replicate the statements

- A statement is not deterministic across devices: `UPDATE … WHERE done = 0`
  matches different rows on each one, and clock or random functions differ.
- A receiver must check every changed row and column against the author's
  grants. A statement does not show which rows it changed.

## Open questions

- What an assignment that is not an increment means on a counter column:
  refuse it, or treat it as a reset that later increments build on.
- Each merge type is a new operation kind on the wire and in checkpoints. It
  needs a decision record and changes the resolved choice of built-in last
  writer wins.
- Whether an ORM-neutral typed call, such as `increment(table, key, column, by)`,
  should exist beside the SQL form for callers that do not write SQL.
- Arbitrary expressions such as `price = price * 1.1` stay value writes. Is
  that boundary clear enough to an application author?
