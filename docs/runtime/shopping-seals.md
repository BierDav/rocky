# Shopping on seals

Implemented. How the [shopping example](../../examples/shoppinglist-app/README.md)
uses [seals, tips and bookmarks](tips-and-bookmarks.md): its seal policy,
compaction, kept seals, branches, offline schema publication and held changes.
The app's code is `HouseholdTips` in `shared/src/data/tips`; the **History** tab
shows it. [Phase 20](../features_09/20-seal-qualification.md) qualifies it.

## Policy

Rocky runs no timer. While the app is visible, each device runs one policy round
every 30 seconds (`HouseholdTips.maintain`):

| Step | Who | What |
| --- | --- | --- |
| Keep | Trusted devices | `keep(KeptSeals(every = true))`; others keep none |
| Seal | Trusted devices following `main` | `seal()` once 100 writes gathered since the last seal |
| Compact | Every device | `compact(keep = 1)`: one seal stays uncompacted unless a later one is final |

Keeping and the list collections' retention are set at open, before any seal:
a seal made before a device keeps seals is kept only while the live tables still
hold its state. A seal that lost a race (`sealed`) or a coordinator that did not
answer (`coordinator_unavailable`) waits for the next round; other failures show
on the History tab.

The list collections (`lists`, `categories`, `items`, `item_categories`) use a
retention window of 0: compacting releases their operations behind the seal once
another device acknowledged them, and a device that missed them imports the
signed base at the seal. A partition follows its collection's policy unless it
has its own ([SDK checkpoints](sdk-bootstrapping.md#retention)).

## Kept seals and views

Trusted devices keep every seal. **History → View** opens Room on the seal with
`setRockySeal(database, RockyBinding.Seal(id))`: the same DAOs, read-only. A view
reads once with suspend DAO queries; Room's change tracking needs a writable
connection, so Flow queries fail on a bound database.

## Bookmarks and branches

The History tab lists the bookmarks that point to a tip (a deleted bookmark stays
in the graph without one) and follows the one chosen. Only the owner and trusted
identities open branches; the household's creator is the owner.

- **Open branch** seals the followed tip if nothing is sealed yet, compacts to
  finalize the latest seal, opens a tip on it with a minimum in-sync size of 1,
  creates the bookmark and follows it.
- A member following a branch whose starting seal it does not hold asks a peer
  that keeps it (`pull`) and switches during a later policy round.
- **Back to main and drop** follows `main` and releases the branch's frames on
  this device, once a peer reported holding its own writes there.
- **Back to main and delete** deletes the bookmark for the household and keeps
  the frames: a trusted device's writes to trusted-only tables on the branch
  reached no other device, so Rocky refuses to drop them.
- **Roll back here** opens a tip on a held seal and moves `main` to it. Writes
  past the seal return as held changes on their devices.

An opening is content-addressed: a branch opened again on the same seal is the
same tip, with the frames written there before.

Devices sync a tip only with devices following it: a phone on `main` receives no
frame of a branch, and each tip has its own coordinated log.

## Coordination

Only trusted devices compose the coordination service. They write coordinated
entries and end a tip's log when they seal it. A member that composed it would
hold every received seal of a coordinated tip outside its graph until it could
confirm the terminal with the coordinator, which only trusted nodes can
([seals and moves](coordinated-writes.md#seals-and-moves)). The founding device
registers the household's trusted identities, not their node keys.

A seal becomes [final](seal-finality.md) only with acknowledgments from nodes
holding an owner-issued trusted grant. The app issues none, so its seals are
never final and compaction keeps one seal uncompacted. For the same reason a
coordinated household cannot publish a migration: its close stays private
until final. A device composing the service also needs its network to open,
so with networking switched off it opens with its network and disables it at
once. Run schema changes in households without a coordinator until the app
issues trusted grants.

## Publishing a schema

`Database.open` publishes the app's schema on the followed bookmark when it is
newer ([schema changes](generations.md#publishing)). The app adds two controls:

- Following a bookmark whose schema is older than the app's (`Follow.Ahead`) on
  a trusted device seals it and reopens, which publishes there. On a branch this
  tries the schema and its migration; nothing of the branch reaches `main`.
- **Networking enabled** persists. A device switched off reopens without a
  network, so a migration it publishes on opening stays on the device until the
  switch is turned on again, which attaches the network.

## Held changes

Writes that cannot cross a migration's closing seal, a lost race or a rollback
come back as held changes. The History tab lists their rows. **Re-author**
writes them again as new writes of today's schema in one transaction: inserts
with fresh ids, updates and deletes by row id; rows of removed tables are left
out. **Drop** discards a change. A device lists every held frame it holds,
also another device's: re-author only changes this device made. See
[held changes](held-changes.md).

## Evidence

`SealsTest` covers the policy, a kept seal's view, a branch only its followers
receive and offline reopening. The [qualification harness](shopping-qualification.md)
runs `seals`, `branch`, `publish` and `race-rollback` across processes; the
[phase 20 evidence](../features_09/20-seal-qualification-evidence.md) records
them and a device run.
