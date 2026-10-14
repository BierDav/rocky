# features_09: evolving schemas without limits

Status: accepted roadmap (2026-10-14). Phases 1 to 3 are implemented; phase 6
is in progress. [features_08](../features_08/README.md) is history, not a
competing next-work authority.

## Destination

A Rocky schema evolves over decades. A new app version may add or remove
tables and fields, and Rocky never limits how old an app version may be and
still sync:

- **A database is exactly one schema version.** When a device learns of a
  newer schema, it migrates its whole database to it, whatever its app
  version expects. There is no in-between state: the database is v6 or v7.
- **Old apps keep working on what is unchanged.** An app older than its
  database keeps reading and writing every table and field the new schema
  kept. Its code that touches removed items fails with ordinary SQL errors;
  the application tells the user to update.
- **Nothing incompatible is sent silently.** Local changes made before the
  migration that touch removed items are held, not sent. A newer app version
  re-authors them as new writes, or drops them.
- **Removal is a deliberate, confirmed step.** The device that first opens a
  version removing something gets an impact report, and the application asks
  the user before Rocky signs it.
- **Migrations have no size limit.** They run in one local transaction on
  an issuer that can read and write every partition of the tables involved,
  and leave as many bundles as they need. They need no coordinator: issuers
  that migrate concurrently converge on the same rows.
- **No remnants.** Manifests are fetched when a frame needs one and dropped
  when no retained frame does. Transitions no longer carry an admission
  horizon, and the cut mechanism of
  [decision 0066](../decisions/0066-breaking-generations.md) is removed.
- **Coordinated writes on ordinary rows.** Writes that must not race, such
  as sequential invoice numbers, are bundles that every connected trusted
  node stores before they become true; the coordinator orders them and
  decides which history is canonical, without holding data. Strong commands
  and command-controlled tables are removed.

Rocky guarantees that every database eventually converges. It does not
guarantee that no data is lost, nor that an app always runs on the complete
schema it declares: an app may be older or newer than its database, and its
queries on what differs fail. An app that must not break forces an update or
handles those failures. Deprecation, update prompts and how long old app
versions are tolerated belong to the application, not to Rocky.

## Settled

- **Names are identities.** A table or field's name is its identity
  forever; there is no separate identity and no translation layer. A name
  removed and added again means the same thing with the same type, which is
  the developer's responsibility. A field whose meaning or type changes takes
  a new name, such as `category2`; renaming is a removal and an addition. An
  app that must keep old versions from misreading a name enforces a minimum
  app version itself.
- **Adopt on discovery.** A device that meets a newer generation fetches its
  schema and publisher-signed manifest, verifies them and migrates its
  database to it with the existing backfill and switch, before it applies
  that generation's frames. No chain of transitions is needed.
- **The same schema everywhere.** Removed tables and fields are dropped on
  every device that runs the new generation, with their constraints; nothing
  is frozen, emptied in place or filled with synthetic values.
- **Re-adding converges.** A generation that re-adds a removed name
  records its signing time as a baseline, the earliest if several issuers
  signed it. Every device ignores writes to the
  field stamped before it, and a device adopting the generation pulls again
  the frames of that field's tables stamped after it. The
  [mergeable horizon](../runtime/mergeable-horizon.md) guarantees the rest:
  every copy that meets a finalized cut either reproduces its state digest
  or imports the finalized checkpoint. Nothing holds back pruning.
- **No device inventory.** Rocky never needs to know how many devices exist
  or whether two of them share a key. Every rule here is applied by each
  copy on its own state; the horizon's participant list only bounds how long
  finalization waits.
- **Receivers check field by field.** A frame from a device that has not
  migrated yet applies its kept fields and ignores removed ones, the same way
  on every device.
- **Held changes are voided, not split.** A held bundle never offered to a
  peer is cancelled with a signed void record, so its author logs continue.
  A bundle that was offered, even by a push that failed midway, counts as
  sent; receivers apply its kept fields. Re-authoring is an ordinary new
  write with a new timestamp; the application can show what it would
  overwrite first.
- **No split brain.** A coordinated write is committed once every trusted
  node in the coordinator's in-sync set stored it, and the set has at least
  the domain's minimum size. A domain whose whole set is permanently cut off
  is stuck until the domain owner signs a recovery cut; Rocky accepts this.
- **The coordinator holds no data.** Per domain it keeps the in-sync set, the
  log head and commit index with a bounded hash window: a few KiB, constant
  over time, with lazy membership and no heartbeats.
- **Old apps may join** a newer household when the protocol is compatible;
  their database starts at the household's generation.
- One current version per format
  ([decision 0039](../decisions/0039-single-current-formats.md)).

## Phases

| Phase | Brief | Depends on | Size | Required result |
| --- | --- | --- | --- | --- |
| 1 | [Names as identities](01-stable-identities.md) | none | S | A name is its table or field's identity forever; no separate identity |
| 2 | [Adopting newer generations](02-adopting-newer-generations.md) | 1 | L | A device migrates its database to any newer generation it meets, whatever its app version |
| 3 | [Removal](03-removal.md) | 2 | M | A generation drops tables and fields on every device that adopts it |
| 4 | [Held changes](04-held-changes.md) | 3 | L | Incompatible local changes are held, voided and re-authored or dropped by the app |
| 5 | [Confirmed rollouts](05-confirmed-rollouts.md) | 3 | M | Removals are signed only after the app confirms an impact report |
| 6 | [No remnants](06-no-remnants.md) | 2 | M | Cuts are removed; unused manifests are dropped |
| 7 | [Migrations at any size](07-migrations-at-any-size.md) | 6 | M | A migration of 100,000 rows runs once and leaves as many bundles |
| 8 | [Coordinated log](08-coordinated-log.md) | none | L | The coordinator orders one log per domain and commits entries the in-sync set stored |
| 9 | [Coordinated writes](09-coordinated-writes.md) | 4, 8 | L | Trusted nodes make race-free writes as ordinary bundles, without split brain |
| 10 | [Stuck recovery](10-stuck-recovery.md) | 9 | M | A stuck domain recovers what any device holds; the owner signs the rest away |
| 11 | [Removing strong commands](11-removing-strong-commands.md) | 9 | L | Commands and command-controlled tables are gone; the examples use coordinated writes |
| 12 | [Qualification](12-qualification.md) | 4, 5, 7, 10, 11 | M | The shopping app removes `items.category` while an older desktop keeps syncing |

```mermaid
flowchart LR
    P1["1 Names as identities (S)"] --> P2["2 Adopting newer generations (L)"]
    P2 --> P3["3 Removal (M)"]
    P3 --> P4["4 Held changes (L)"]
    P3 --> P5["5 Confirmed rollouts (M)"]
    P2 --> P6["6 No remnants (M)"]
    P6 --> P7["7 Migrations at any size (M)"]
    P8["8 Coordinated log (L)"] --> P9["9 Coordinated writes (L)"]
    P4 --> P9
    P9 --> P10["10 Stuck recovery (M)"]
    P9 --> P11["11 Removing strong commands (L)"]
    P4 --> P12["12 Qualification (M)"]
    P5 --> P12
    P7 --> P12
    P10 --> P12
    P11 --> P12
```

## Reserved decision records

| Record | Phase | Topic |
| --- | --- | --- |
| 0067 | 1 | [Names are table and field identities](../decisions/0067-names-are-identities.md) |
| 0068 | 2, 3 | Adopting newer generations and removal |
| 0069 | 4 | Held changes, void records and re-authoring |
| 0070 | 5, 6 | [Confirmed rollouts and the end of cuts](../decisions/0070-confirmed-rollouts-and-the-end-of-cuts.md) |
| 0071 | 7 | Staged migration publication |
| 0072 | 8, 9 | The coordinated log and coordinated writes |
| 0073 | 10, 11 | Stuck recovery and the end of strong commands |

## Open questions

None at the moment; each brief settles its own details.

## Agent execution contract

Give an agent this README and one phase brief. Read root `AGENTS.md`,
[development](../development.md), the [module map](../modules.md),
[generations](../runtime/generations.md) and
[decision 0066](../decisions/0066-breaking-generations.md). Verify the brief's
prerequisites against code before editing; if one fails, repair it within
scope or report the exact blocking contract. Never weaken admission,
durability or atomicity, and never report success for missing behavior.

Update the owning guides, the brief's status line and its evidence with each
completed slice. Use `jj`, run `./scripts/lint` and `./scripts/check`, and for
Kotlin changes `sdks/kotlin/gradlew -p sdks/kotlin check` and
`./examples/shoppinglist-app/check`. Report what passed and what was not
verified.
