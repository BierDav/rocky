# 17. Schema changes through seals

Status: proposed; not started.
Owner: `rocky-core` generations; `rocky-secure` generation switch and
admission; `rocky-sdk` generations and migrations. Depends on
[phase 16](16-kept-seals-and-views.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision
record 0075.

## Inputs and objective

Read [the generation model](README.md#generations-as-sealed-forks),
[generations](../runtime/generations.md),
[adopting generations](../runtime/adopting-generations.md),
[removal](../runtime/removal.md) and
[migrations at any size](07-migrations-at-any-size.md).

After this phase a schema change is a seal whose successor tip has another
schema. Generations no longer form a chain of transitions, and both
[accepted limitations](README.md#accepted-limitations) are gone.

## Settled

- **A schema change is a seal.** Without migration steps, `continue`
  names the successor's schema. With them, the issuer seals with `close`,
  opens a tip with the new schema on that seal and moves the bookmark; the
  migration runs as the new tip's first signed writes, as in
  [phase 7](07-migrations-at-any-size.md), and reads the parent through a
  [read-only view](16-kept-seals-and-views.md) of the sealed parent.
- **A migration admits no late frames.** Frames beyond a closing seal's
  frontier return as held changes, so nothing reaches the new tip around
  the migration. A `continue` seal admits late frames field by field, as
  [phase 14](14-seals-and-bookmarks.md) does for an unchanged schema.
- **Additive switches touch no rows.** Adding a table or a nullable or
  defaulted field changes the installed schema in place; no shadow
  backfill, rebuild or observer notification. Removals and migrations keep
  the existing switch.
- **Splits are concurrent seals.** Issuers that seal one tip with
  different schemas, or run one migration twice, resolve
  [by authority, then digest](14-seals-and-bookmarks.md); the other tip
  stays a branch, its migration never merged.
- **Trying, then publishing.** A developer tries a schema change and its
  migration on a dev branch. Nothing of the branch is reused: to publish, an
  issuer seals `main` forward and runs the migration on that fresh parent.
  To have the migration ready when the seal becomes public, it migrates
  with networking disabled and publishes the seal, opening, move and
  migration bundles together when it reconnects, as
  [staged publication](../decisions/0071-staged-migration-publication.md)
  does. Writes others make to `main` meanwhile follow the late-frame rules.
- **Old apps across a migration.** An older device's writes across a
  closing seal return as held changes. Re-authoring them is the developer's
  burden: an app kept forward compatible, a trusted node that re-authors
  them, holding them until the app updates, or an update forced before the
  change. Rocky does not choose.
- **Following, not discovery.** A device switches schema when its followed
  bookmark reaches a tip with another schema, never because a peer runs
  one.
- **Removed:** signed transitions and their lineage, the lineage caps,
  learning older manifests, re-add baselines and the pull after a baseline.
  A device needs the schemas of the tips it writes, keeps or reads frames
  from. The removal impact report and confirmation of
  [phase 5](05-confirmed-rollouts.md) stay.

## Ordered slices

1. Seals and tips carry a schema; following switches to it.
2. Migrations read through views; the migration rule for late frames.
3. Additive switches in place.
4. Removing transitions, lineage, manifest learning and baselines.
5. Guides: generations, adopting generations, removal; the accepted
   limitations of the [README](README.md).

## Completion gate

- An older device, offline across a migrating seal, gets its writes back as
  held changes; its app re-authors them and every device converges.
- An offline device's writes across an additive seal apply on every device.
- Adding a nullable field to a table of 100,000 rows rewrites none of them.
- Two issuers sign different schemas, or run the same migration, at once;
  every device ends in one tip with no duplicated rows.
- An issuer seals and migrates offline while other devices write `main`;
  on reconnecting, every device on `main` moves to the new schema with the
  migration's rows, and the writes made meanwhile return as held changes.
- A device on `main` never fetches or installs the schema of a branch it
  does not follow.
- A name removed and added again in a later generation never receives a
  value written before its removal.
