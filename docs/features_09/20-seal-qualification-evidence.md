# Phase 20 evidence

See the [brief](20-seal-qualification.md) and the app guide,
[shopping on seals](../runtime/shopping-seals.md). Recorded 2026-10-22 on one
macOS arm64 host.

## Completion gate

| Gate item | Result |
| --- | --- |
| Every device ends with the same lists after each scenario | Harness `seals`, `branch`, `publish`, `race-rollback` pass; each compares every device's lists. Device run below: all four devices equal after every slice |
| After compaction, no device's file holds tombstones of sealed tips it compacted and does not keep | Harness `seals`: member and phone compact at the second of three seals, release 6 operations (8 → 2), keep 0 tombstones and drop the two compacted tips from the tips they hold. Device run: the emulator's file after compacting holds 2 operations, 0 tombstones; the member released 9 |
| While `dev` is written, the phone receives none of its frames and no `dev` entry reaches `main`'s coordinated log | Harness `branch` over three loopback voters: `main`'s log stays at 1 while `dev`'s own log commits its decision at 1; the phone holds frames of no dev tip and only main's decision. Device run: of the emulator's 5 stored frames, 3 name `main`'s tip and none `dev`'s |
| `./examples/shoppinglist-app/check` passes; timings recorded | Passes (JVM and Android tests, required schema against Room's version 8 export, desktop and Android builds). Timings below |

## Timings

Single samples on this host; loopback for the harness, n0 relays and discovery
for the device run.

| Step | Harness | Device run |
| --- | --- | --- |
| Seal call | 7–9 ms | — |
| A member adopts a seal | 352–363 ms | Emulator: about 4 s after its networking was switched back on |
| Compaction round (`compact(keep = 1)`) | member 16–24 ms, phone 17–28 ms | — |
| Read a kept seal through a bound view | 78–82 ms | — |
| Offline migration to schema 8 at open | 200 ms | 0.35 s including the open |
| Members adopt the published schema after reconnecting | 1,074 ms | 3.8 s |
| Devices follow the owner's rollback | — | 4 s |

## Device run

Devices: an Android emulator (Pixel 9, Android 17) running the app version 7
APK, driven through its UI with `agent-device`; desktop JVM processes running
the app's shared code through the harness's `device` mode: the trusted
desktop (app 7, then 8), a member desktop (app 7) and, for rollback, an issuer
(app 7). Peers reached each other through n0's relays and discovery. Slices 1
and 2 ran in a household with three loopback voters; slices 3 and 4 in one
without a coordinator (see [limits](#limits)).

1. **Seals.** Three seals, the policy making the second. The emulator, its
   networking switched off in **Devices**, stayed on the second seal, then
   adopted the third and compacted at the second by its own policy. The
   trusted desktop read two and three seals back through bound views.
2. **Branch.** A decision on `main` (log 1); `dev` opened on `main`'s latest
   seal; the member followed it; both wrote; a decision on `dev` (its log 1).
   The emulator added an item to `main` through its UI and never received a
   `dev` write. Back on `main`, its log still read 1; the compacted member
   dropped `dev` (9 → 5 operations) once the drop fix below landed.
3. **Schema 8.** App 8 migrated `dev`; the emulator stored no new schema.
   Following `main` switched after one sync pass, reported ahead, sealed and
   migrated with no network. The emulator (UI) and member wrote meanwhile; on
   reconnecting both reached schema 8, and their writes came back held. The
   member re-authored its own; the emulator's **History → Re-author** did its.
4. **Rollback.** With the owner offline, the issuer sealed `main` forward
   while the owner moved it back to its latest kept seal. Every device
   followed the owner within 4 s; the emulator's History lists `main/1`, and
   its write past the seal came back held.

Evidence notes are in `local/p20/device-evidence.txt` (not committed).

## Repairs below the app

Each has a regression test and changes the owning guide.

| Defect | Repair and test |
| --- | --- |
| Partitions had no retention policy, so compaction released nothing of a shared list and reported nothing | A partition follows its collection's policy; `compacting_releases_a_shared_lists_history_under_its_collections_policy` |
| A node keeping seals failed to migrate a partitioned table: the rebuild inside the switch wrote history the new schema had not installed | The switch records its own difference; `an_issuer_keeping_every_seal_migrates_its_lists` |
| An issuer whose published schema lost a race stayed on a pending switch forever | A losing source continues to nothing; `issuers_that_publish_different_schemas_apart_end_on_the_winners` |
| A compacted device could never drop a branch | Refuse only history that may hold the tip's frames; `a_compacted_device_drops_a_branch_it_wrote_on` |
| After trying a migration on a branch and returning, finalizing `main` replayed the branch's frames and failed | Replay skips frames off the target's path; `an_owner_returns_from_a_migrated_branch_and_compacts_main` |

App repairs: coordination registered node keys instead of identities; members
composed the coordinator and then held every coordinated seal back; trusted
devices that pruned history could no longer close a tip (migrate, roll back).

## Limits

- **Physical phone not run.** The Pixel 11 Pro was attached but locked; this
  run used the emulator. The physical two-phone procedure stays outstanding.
- **Desktops ran headless.** `agent-device` reads the Compose desktop window
  but cannot act on a JVM process without an app bundle, so the desktop
  devices ran the shared app code, not its UI.
- **Coordinated households cannot publish a migration.** A coordinated seal or
  migration becomes final only with acknowledgments from nodes holding an
  owner-issued trusted grant, and a database composing the service cannot
  open without its network. The app issues no trusted grants: in the device
  run, the try-out migration stayed private and the switch back stayed
  pending. Slices 3 and 4 therefore ran without a coordinator.
- **Concurrent publication ran in the harness only.** Two issuers publishing
  different schemas needs a rival build; `race-rollback` covers it.
- Harness `gate` and `creator-loss` fail identically on the code before this
  phase; they are recorded in [shopping qualification](../runtime/shopping-qualification.md#known-failures).
- `ApprovalsTest` failed once under the full `check` load (`Undecided`) and
  passed three times alone.
