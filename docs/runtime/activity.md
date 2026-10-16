# SDK background activity

`RockyDatabase.activity()` and `RockySession.activity()` expose a
`Flow<RockyEvent>` describing background work: peer attempt outcomes,
changed sets of scopes a peer refused, failed result pulls, nonempty transfers, checkpoint imports, bootstrap,
enrollment, renewal, revocation, author assignment and partition key/holding
changes. Events follow successful local work; they are diagnostic observations,
not durable receipts or proof that every peer has the latest data.

```kotlin
scope.launch {
    database.activity().collect { event ->
        when (event) {
            is RockyEvent.Synced -> showTransfer(event.peer, event.pulled, event.pushed)
            is RockyEvent.CheckpointImported -> showCheckpoint(event.scope, event.checkpoint)
            is RockyEvent.PartitionChanged -> showPartition(event.action, event.group, event.values)
            else -> showActivity(event)
        }
    }
}
```

## Lifetime and bounds

The database retains the latest 512 events in memory, in sequence order.
A new collector receives retained events before waiting for new ones. Sequence
numbers start at one per open database; timestamps are local Unix milliseconds.
`pairing(profile)` returns its invitation event's sequence as well as the URI
and expiry. This lets an application associate later enrollment events with
the invitation choice even if a newer invitation is created before collection.
Closing or restarting the database discards its feed. Cancelling one collector
leaves other sessions, observers and networking running.

A slow collector can miss events older than the retained window. Sequence gaps
expose that loss; there is no persisted audit trail or replay across opens.
The Kotlin adapter buffers a bounded channel. Idle sync passes produce no
transfer events; unchanged peer failures and repeated bootstrap starts are
suppressed. Successful earlier parts of a sync that later fails may not appear
as transfer events. Current status, holdings and network status remain the
source for the current device state.

`PeerReconciliationCompleted` (`peer_reconciliation_completed`) records a
completed exchange, and `PeerAttemptFailed` (`peer_attempt_failed`) records a
failed attempt. Neither is a reachability state. `link_attempt_failed` records
a failed link-opening attempt.

`LinkOpened` (`link_opened`) records a peer with reason `dialed` or `accepted`.

The activity kinds `networking_enabled` and `networking_disabled` record the
application's networking control. Kotlin `RockyEvent.NetworkingControl.enabled`
carries the value. They do not measure internet reachability.

`LinkClosed` (`link_closed`) records `networking_disabled`, `suspended`,
`worker_disabled`, `detached`, `authority`, `duplicate`, `busy` or `error`.
An error close includes the diagnostic. Link transitions wake status waits;
path measurements emit no activity.

`ScopesRefused` (`scopes_refused`) lists the scopes a sync with a peer skipped
because one side may not hold them, typically a list the peer has not been
shared yet ([decision 0046](../decisions/0046-per-scope-frontier-refusal.md)).
It is recorded when that set changes for the peer; an empty list means nothing
is refused any more.

`ScopesWaiting` (`scopes_waiting`) lists the scopes a sync pushed no history
of because the peer restores them from a checkpoint: it is
[importing](checkpoint-import.md#while-a-scope-is-importing) them, or is behind
this device's pruned prefix. The sync continues with the other scopes. It is
recorded when that set changes for the peer; an empty list means nothing waits
any more.

`ResultsFailed` (`results_failed`) carries a peer and an error when a sync moved
rows but pulling confirmed command results failed, for instance because the
coordinator no longer verifies a receipt. The pass is retried with the next
sync. It is recorded when the error changes, not on every failed pass.

`finality_failed` carries an error when this device could not acknowledge
an ended tip log or publish a [finality record](seal-finality.md); the next
publication, decided entry or sync pass retries.

`registry_failed` carries an error when an issuer could not read or update
its [issuer registry](issuer-registry.md) outside an answer, such as while the
registry's rows wait for a rotated key. Sync admission does not read the
registry; answers that need it fail until it reads again.

A transfer names the table and, for a partition, its encryption commitment.
`GroupPartition.status()` reports the same per-table partition identifier so an
application can label events with its own list/customer name. Keys and
application row payloads are excluded from events.

The [shopping app](../../examples/shoppinglist-app/README.md) combines this feed
with application key-delivery notes and current
[network status](sdk-composition.md#network-status). Inbound session counters
show service activity on a device that serves peers rather than initiating
outbound sync. Transfers in the feed describe this device's outbound passes.

## SDK requests

The current [SDK envelope](sdk-contract.md) accepts:

| Request | Result |
| --- | --- |
| `activity`, optional `after` sequence as a string | Up to 128 `events`, and `token` as a sequence string |
| `activity_next`, `after` sequence as a string | The next page, waiting until an event exists or the session closes |

Each event carries `sequence`, `at` and `kind` plus its detail fields.
Times and sequences are decimal strings in the native JSON boundary. Keep the
returned token for pagination. Activity is shared by sessions on one database.

## Verification

`rocky-sdk` unit tests check ordered paging, bounded retention and suppression
of duplicate events. `shared_lists` checks author installation through the
renewal lane, reopening a checkpoint-received list with its child rows
visible immediately, and two issuers refusing each other's unshared list
until it is shared. Kotlin household tests observe installed-key events.
