# 16. Kept seals and read-only views

Status: proposed; not started.
Owner: `rocky-secure` workspace, history tables and views; `rocky-storage`
connections; `rocky-sdk` and `sdks/kotlin` configuration and connections.
Depends on [phase 15](15-lazy-compaction.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision
record 0075.

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[SQL driver](../runtime/sql-driver.md) and
[queries and observation](../runtime/queries-and-observation.md).

Migrations, audits and debugging need to read earlier states of the domain.
After this phase a node keeps the seals it is configured to keep, and any
connection can read a kept seal or a bookmark's seal through read-only views
shaped exactly like the live tables.

## Settled

- **One live workspace.** A domain's live tables hold the tip the followed
  bookmark points to, under their plain names, in the same file and
  transaction as history, as today. Switching bookmarks rebuilds them.
- **Workspaces are a boundary.** Live tables, their cursor into history
  and their observers are reached through a workspace handle, never by
  assuming one. The current runtime supports exactly one; adding more, in
  other files or on [Postgres](../ideas/support_postgres_as_full_node_backend.md),
  must not require changing this phase's code. See
  [additional live workspaces](../ideas/additional_live_workspaces.md).
- **Kept seals.** Per-node configuration names the seals to keep: those
  bookmarked tips start from, the latest N, or every one, among the tips
  the node follows. A node keeps a seal only
  once it holds every frame through its frontier; before that the seal is
  shallow there.
- **History tables.** Each application table has one history table. A
  change to a live row writes its previous version there, in the same
  transaction, while a kept seal still needs it. A kept seal's state is the
  live rows unchanged since it plus the history rows valid at it.
  Releasing a seal from the configuration deletes the versions only it
  needed; the seal stays in the graph, shallow.
- **Schema changes.** A kept seal before an additive change selects fewer
  columns. Before a removal or migration it keeps a frozen copy of only the
  affected tables.
- **Read-only views.** A connection bound to a kept seal or a bookmark
  creates temporary views with the live tables' names and shape, filtering
  support rows. Unlimited connections may be open at once; they are
  read-only and need no change notifications, since a sealed tip never
  changes. An unbound connection names history explicitly.

## Ordered slices

1. The workspace boundary in `rocky-secure`, with one workspace.
2. Kept-seal configuration and history tables.
3. Frozen copies for removals and migrations.
4. Seal- and bookmark-bound read-only connections in `rocky-storage`,
   `rocky-sdk` and Kotlin, including the Room profile.
5. Guides: kept seals and views; the additional workspaces idea.

## Completion gate

- A Room DAO runs unchanged against a connection bound to a seal kept two
  seals ago and returns that seal's rows.
- Keeping five seals of a table where 1% of rows change between seals adds
  about 5% to that table's storage.
- A seal before a field's removal still shows the field.
- A seal learned before its frames arrived is reported shallow until they
  do.
- Writes through a read-only connection fail with SQLite's read-only error.
- Switching from `main` to a branch two seals back and returning costs
  time proportional to the rows changed since that seal, not to the table.
