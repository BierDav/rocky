# 7. Migrations at any size

Status: proposed; not started.
Owner: `rocky-sdk` `database.rs` migration flow and status; `rocky-secure`
capture, outbox and bundles; `sdks/kotlin`. Depends on
[phase 6](06-no-remnants.md). Follow the
[execution contract](README.md#agent-execution-contract). Takes decision record 0071.

## Inputs and objective

Read [generations](../runtime/generations.md),
[secure sessions](../runtime/secure-sessions.md) and
[SQL sessions](../runtime/sql-sessions.md).

A schema's `migrations` run in one SQL transaction published as one bundle,
so a migration changes at most 2,048 rows (`MAX_BUNDLE_ROWS`). Filling a new
field of a table with 100,000 rows is impossible. After this phase a
migration has no size limit: it still runs as one local transaction, and its
result is published as many bundles.

## Settled

- **Who migrates.** Only an issuer whose grants cover every partition of
  every table its migrations read or write, for reading and writing, may run
  them and sign the generation, as the current check over
  `settings.partitions` already requires. Another device opening that app
  version keeps its database at the household's generation; its app's
  queries on what the new version adds fail, and status says an issuer with
  full access must migrate first.
- **Order.** The issuer signs the generation first; its signing time is the
  baseline. Then, at open and before the network starts, it runs every
  migration step in one local SQL transaction of any size. That transaction
  also stores the net change as a staged publication and marks the migration
  done, so a crash either repeats nothing or loses nothing.
- **Publication.** The staged change leaves as consecutive bundles within
  the bundle limits, parents before children. Each bundle's handle derives
  from the generation and its position, so a retry returns the same frames.
  Publishing resumes after a restart until the stage is empty.
- **Stamps.** Every migrated write carries the stamp of the local commit,
  after the baseline. An edit another device makes while bundles are still
  leaving is newer and wins.
- **Receivers.** Each bundle is ordinary. Until the last arrives, rows not
  reached yet read their default; the hidden-row rules settle references and
  unique values in between. Every device ends with the same rows.
- **Rows not held.** Rows the issuer did not hold when it migrated keep
  their default. Rocky guarantees convergence, not that a migration saw
  every row.
- **Steps unchanged.** SQL and `insert` steps keep their form and lose their
  size limit. Ordinary application transactions keep theirs.

## Ordered slices

1. Decision record 0071: staged migration publication and its handles.
2. Sign-then-migrate flow at open, with the authority check and the
   durable done mark.
3. Capture beyond the bundle limits for migrations only; staging, chunked
   publication, resume.
4. Status: publication progress; "an issuer with full access must migrate".
5. Guides: generations, secure sessions, SQL sessions.

## Completion gate

- An issuer adds a required field to a 100,000-row table and fills it with a
  migration; every member converges on the same values.
- A crash after the local migration commit resumes publishing without
  repeating or duplicating a row; a crash before it repeats the migration.
- An edit a member makes while bundles are still arriving wins on every
  device.
- An issuer without access to one partition cannot sign; a device whose app
  declares the new version keeps syncing on the old generation, and its
  queries on the new field fail with SQL errors.
- Measured publication time and size are recorded with the evidence; the
  large case runs in the tier [testing](../testing.md) assigns it.
