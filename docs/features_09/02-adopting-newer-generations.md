# 2. Adopting newer generations

Status: proposed; not started.
Owner: `rocky-secure` replica generations, admission and projection switch;
`rocky-sdk` `generation.rs`, `database.rs` open, bootstrap and the sync
worker; the driver's Room profile. Depends on
[phase 1](01-stable-identities.md). Follow the
[execution contract](README.md#agent-execution-contract). Shares decision record 0068.

## Inputs and objective

Read [generations](../runtime/generations.md),
[secure sessions](../runtime/secure-sessions.md),
[SDK bootstrapping](../runtime/sdk-bootstrapping.md) and
[SQL driver](../runtime/sql-driver.md).

Today a device adopts a newer generation only when its app opens with the
newer schema. Until then `Inner::lookup` in
`crates/rocky-secure/src/replica/generation.rs` refuses the newer manifest,
and the SDK reports `upgrade_required` and holds every local change. After
this phase a device migrates its database to any newer generation it meets,
whatever schema its app declares, and keeps syncing.

## Settled

- **Fetch and verify.** A device that meets an unknown newer manifest fetches
  the generation's schema and publisher-signed manifest from the peer that
  served it, as members fetch an issuer's offer today, and verifies them.
  No chain of transitions is needed; ids are stable identities.
- **A generation is its schema.** Transitions to the same schema hash,
  signed by issuers that could not reach each other, are one generation. Its
  baseline is the earliest signing time among them, on every device; a
  device that learns an earlier one pulls again the frames stamped after it,
  as [removal](03-removal.md#settled) does for a re-added field.
- **Migrate first.** The device migrates its database to that generation
  with the existing [backfill and switch](../runtime/generations.md#plan-backfill-and-switch),
  durably and resumable, before it applies that generation's frames. It never
  runs the schema's `migrations`; their results arrive as signed writes.
- **App newer than the database.** Opening with a newer app schema signs and
  adopts it when this device may migrate it
  ([phase 7](07-migrations-at-any-size.md)) or an issuer already signed it;
  otherwise the database stays at the household's generation and status
  says so.
- **App older than the database.** Opening with an app schema older than the
  installed one opens the installed database unchanged. Status reports that
  the app is behind; sync continues.
- **Room.** The driver's Room profile answers `PRAGMA user_version` with the
  version the app declares, so a Room app older or newer than its database
  still opens; its DAOs that touch what differs fail with SQLite errors.
- **No admission horizon.** Transitions stop carrying `horizon`; today every
  transition sets it to the previous generation, which refuses devices two
  versions behind. Every generation of the lineage is admitted.
- **Horizon statements.** A statement names its manifest. A device on an
  older generation adopts that generation before it acknowledges or
  finalizes. A copy whose state differs from a finalized cut, participant or
  not, imports the finalized checkpoint, as today.
- **Older frames.** Frames of devices that have not migrated yet are admitted
  under their own manifest, fetched on demand like a newer one.
- **Old apps join.** Bootstrap installs the household's current generation,
  whatever the app declares, when the protocol is compatible.

## Ordered slices

1. Decision record 0068 (with [removal](03-removal.md)); fetching, verifying
   and recording a newer generation; no admission horizon.
2. Migrating on discovery before applying its frames; resuming after a crash.
3. Opening with an older app schema; status; the Room profile's version.
4. Older manifests on demand; joining at the household's generation.
5. Guides: generations, SDK bootstrapping, SQL driver, Room integration.

## Completion gate

- A device whose app declares v6 meets an issuer on v9: its database becomes
  v9 in one switch, it syncs every table both ways, and reopening with the v6
  app opens the v9 database.
- A crash during that migration resumes on reopen and ends at v9.
- An old Room app opens the migrated database; a DAO on an unchanged table
  works and one on a removed column fails with a SQLite error.
- A forged schema or manifest is refused before anything changes.
- Two issuers that sign the same schema apart end on one generation with the
  earlier baseline on every device.
- A device on v8 that receives a v9 horizon statement migrates to v9 before
  acknowledging it.
