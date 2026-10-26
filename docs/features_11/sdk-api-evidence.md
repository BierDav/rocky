# Kotlin application API

Status: implemented; automated SDK and shoppinglist checks passed (2026-10-26).
This work addresses the application API review alongside the reliability phases;
it does not qualify their device or distributed-protocol gates.

## Implemented behavior

- Related SQL and a partition move publish as one bundle. A failed move or local
  commit leaves the related rows intact; replay checks the complete input.
  A regression checks that source links name the deletion frame even when related
  inserts span multiple frames; updates and deletions precede those inserts.
- Room partition creation reserves Room's writer before preparing keys outside
  the SQL transaction, then reserves and validates the identity inside it.
  Competing reservations retry before application code runs. Callback failure
  rolls back the insert and reservation; calls inside an existing transaction
  are refused before preparation.
- SQLite failures carry structured read-retry metadata. Managed Room observation
  reports temporary unavailability, retains the last value, and propagates
  permanent failures. Resource ownership and cancellation cleanup are explicit.
- Configuration supplies the same networking policy to create, open and join.
  Credential observations expose registry state and durable invitation enrollment
  associations, including associations received through registry sync. Batch
  partition observation shares status and activity waits across all requested
  groups, including delivery metadata changes that leave the status token intact.
- SDK runners reconcile desired key recipients, maintain coordination
  registration and renewal, and apply history policy. Applications supply policy
  and durable shared coordination metadata. Deferred bookmark following persists
  across restart and requests its missing seal through native recovery.
  Registration expiry comes from the native request's persisted signing time,
  so an uncertain retry cannot postpone the renewal deadline locally.
- Coordinated queries, retry categories, retention policy and generation state
  have typed Kotlin results. Uncertain coordinated writes retain their operation
  handle for reconciliation rather than automatically repeating application code.
- Shoppinglist uses the packaged Room export plus Rocky policy overlay, atomic
  move composition, partition creation, configuration, managed Room acquisition,
  and SDK policy runners. Its delivery view observes current partition state.

Low-level factories and direct session credential methods remain public alongside
the configured path. Room entities still declare their row-kind properties,
identity indexes and ID-allocation DAO methods; this work adds no generator for
those declarations.

Enrollment settings use format 20. Earlier settings are refused; no migration or
legacy reader is provided.

## Qualification

No test timeout, tier budget or admission rule was relaxed for this change.

On base `a56ce100`, `./scripts/check` passed 203 unit tests and 1,411
integration tests across the workspace and the SDK without default features.
All tier and individual timing budgets passed. The cached-session recovery path
now recognizes typed authority invalidation; the formerly failing coordinated
branch-position regression passed in both SDK feature configurations.

`./scripts/lint` and `./scripts/commitlint --jj` passed.
`sdks/kotlin/gradlew -p sdks/kotlin check publishToMavenLocal --continue
--no-parallel --max-workers=2` passed: core, SQLite and Room on JVM and iOS
Simulator, the packaged JVM consumer, native schema installation, and local
artifact publication.

`./examples/shoppinglist-app/check` passed: packaged Room schema installation,
all 70 JVM tests, and desktop and Android application builds. The partition
creation regression also passed alone: eight concurrent creators receive
separate identities, callback failure runs once and rolls back, and a following
creation succeeds. The full run also passed revocation/rejoin, returning from a
branch to bootstrap a new list, and observation while a full peer is offline.
These passed against fresh publications after clearing Amper's generated
Android AAR extraction cache; no project cache workaround was added.

Physical Android/iOS devices, the three-device workload, and packaged iOS
consumer qualification have not been run for this API work.
