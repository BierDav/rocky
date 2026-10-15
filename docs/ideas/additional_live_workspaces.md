# Additional live workspaces

Status: idea; not planned.

[Kept seals and views](../features_09/16-kept-seals-and-views.md) gives a
domain one live workspace: the writable live tables of the bookmark a node
follows, stored with history in one file. Every other seal is reachable
through read-only views. Some uses need a second writable workspace:

- a sandbox or draft on the same device that keeps working on `main`;
- following and writing a bookmark other than `main`, a shared branch;
- a backend that serves several writable bookmarks of one domain.

The sealed-fork variant of the same-device case, including the per-branch
author log it needs, is [simultaneous branches](simultaneous_branches.md).

## Idea

History stays the source of truth. Each additional workspace holds its own
live tables and a cursor into history. A write commits its frames and outbox
to history first, then its projection to the workspace; opening a workspace
replays what history holds beyond its cursor. History never prunes a frame a
workspace has not applied.

## Costs

- On SQLite, WAL makes a transaction across attached files atomic per file
  only, so each additional workspace depends on the replay path above.
  Workspaces share history's single writer.
- Creating a workspace builds its live tables from history, proportional to
  the data, unless a file-system clone is available.
- A forgotten workspace pins history until it is deleted.

On a [Postgres backend](support_postgres_as_full_node_backend.md) each
workspace is a schema in one database: one transaction spans history and
every workspace, writers run concurrently, and `search_path` binds a
connection to a workspace without rewriting queries. Creating one still
copies its live tables.

## Open questions

- How a connection binds to a writable non-default workspace on SQLite,
  where tables cannot share plain names in one file.
- Whether merging a workspace back into `main` by re-authoring belongs here
  or in its own idea.
