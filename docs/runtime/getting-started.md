# Getting started: a Kotlin app on a coordination service

Qualified for the JVM desktop profile on loopback; see the
[end-to-end evidence](end-to-end-evidence.md). No public endpoint is deployed.
This guide composes the pieces; each links to its own reference.

An application consists of Kotlin schemas, command declarations and deterministic handlers. It links
the [packaged SDK jars](sdk-packaging.md) and needs no Rust project or native
build, and no backend of its own. Devices sync mergeable data directly with each other. A
provider's coordination service orders the strong commands. It holds metadata
only. `examples/shoppinglist-app` is a complete example;
[`examples/market-app`](../../examples/market-app/README.md) implements the market these steps define.

## 1. Define the application

```kotlin
val Reserve = Command("Reserve", handler = "market.reserve.v1", arguments = reservation,
    accepted = remaining, rejected = shortfall, invocation = "holds", result = "holds",
    resources = listOf("holds", "stock"))
val schema = Schema("market", 1, listOf(listings, memos, stock, holds), CommandSet(listOf(Reserve, Restock)))
val handlers = listOf(Reserve handledBy { context, request -> /* decide from context.rows(...) */ })
```

Mergeable tables merge per field, last writer wins. Use a command only for a transition that
must not merge, such as taking the last item. See [typed commands](typed-commands.md).

## 2. Open devices

```kotlin
val runtime = CommandRuntime(ServiceCoordinator(bundleJson), handlers)   // executors
val db = RockyDatabase.create(dir, schema, PeerTransport.Iroh(), commands = runtime)
val member = RockyDatabase.join(dir2, schema, invitationUri, commands = CommandRuntime(ServiceCoordinator(bundleJson)))
```

`bundleJson` is the provider's [trust bundle](public-coordination.md#configure-a-device).
A device that only invokes commands composes no handlers. Invite with a grant
profile (`Reader`, `Writer` or `Issuer`), then keep one `contact()` with `addPeer`. Authenticated sessions hand over
other contacts and command calls request the executor route when needed. A local database works
with no network and no coordinator: `RockyDatabase.local(dir, schema)`. To share
your own iroh endpoint, use `PeerTransport.Endpoint(RockyEndpoint.bind(...))`; see
[iroh](sdk-transports.md#iroh).

## 3. Register the domain

The owner holds a 32-byte tenant control key, kept like an account secret. It
coordinates metadata only and grants no right in the app. On an issuer:

```kotlin
val owner = db.service(tenantKey)
owner.register(registration = persistedId, executors = listOf(db.commandIdentity()),
    invokers = listOf(db.commandIdentity(), memberIdentity))
```

`CommandIdentity.encoded` and `decode` move another device's identity to the owner.
Persist every `registration` and control `id` before calling, and retry with the same
value. Change executors with `owner.control(revision, id, executors, invokers)`.
That fences the current executor and keeps the head. The new executor acquires the
domain on its own next `run`. Until it has, invokers get the retryable
`executor_unavailable`. After a declared upgrade, the upgraded executor calls
`owner.activate(id)`. See [public coordination](public-coordination.md).

## Trusted execution

The executor decides each command with your handler, from the confirmed rows and the
arguments. Every device trusts the executors named in the domain's control and in its own
credentials. Handlers must be deterministic; the runtime cannot prove that. Voters
are trusted not to be Byzantine. They see scopes, identities, commitments, timing and
sizes. They never see schemas, handlers, arguments, results or rows. A device that is not
the assigned executor refuses delivered commands, and a fenced one answers without deciding.

## Public retention windows

| Window | Default | After it |
| --- | --- | --- |
| Admission | 24 h after the identity's issue time | Never confirmed: the ticket reads `Expired` and never executes |
| Response horizon | 7 days, or earlier once the invoker acknowledged | Coordinator lookup ends: an unknown ticket reads `Retired`, outcome unavailable there |

Run the same ticket again after any uncertainty; never submit a replacement, and
never treat `Expired`/`Retired` as success or failure of the business intent. Keep
results and receipt evidence on your devices: after the horizon only they prove an
outcome. A provider's genesis states its exact limits. See
[coordinator retention](coordinator-retention.md).

## Outages

| Situation | Behavior |
| --- | --- |
| Leader voter lost | Re-election; strong work continues |
| Quorum lost | Tickets stay `Queued`/`OutcomeUnknown` with retryable `unknown_confirmation`; mergeable writes and peer sync continue |
| Quorum restored | The same ticket completes once, if within its admission window |
| Executor down | Invokers' tickets wait with `unreachable`; the same tickets complete after restart or takeover |
| Executor lacks confirmed results | Retryable `missing_head`; it syncs and retries, never inventing state |

## Optional custody

The coordinator stores commitments, not payloads. Confirmed results exist only on
devices that hold them. Enroll at least one custodian: an authorized device, such as
a second issuer, that syncs and retains results. Each result travels with its
voter-signed evidence, so a custodian is useful after the coordinator retired the
identity ([design rationale](encrypted-commands.md#portable-result-evidence)).
If the only copy is destroyed, strong progress for that domain stops explicitly
(`missing_head`), while mergeable writes continue. Rocky never reconstructs the result
from a commitment. Enroll custodians before revoking devices: a device enrolled after
a revocation cannot sync earlier mergeable rows ([limits](credentials.md#limits)).

## Share lists without a coordinator

A mergeable-only app needs no coordinator. The [shopping example](../../examples/shoppinglist-app/README.md)
puts a list, its categories and items in one partition group, so one secret per
list derives every table's key:

1. `partitions.next("lists")` prepares a new list's secret; insert the list in
   the same transaction that reserves its id.
2. `db.list(id).share(nodes)` seals the held secret to each recipient. The SDK
   delivers and installs it through direct sessions or a custody mailbox and
   bootstraps the list from a checkpoint. No key table is involved. In shopping,
   members scan a recipient contact; trusted nodes pick from `devices()`.
3. Run the packaged [custody peer](always-on-peer.md) with the same schema JSON
   and keep its contact on each device. Phones that are never online together
   sync through it; it learns new lists and reads nothing. Reciprocal contact
   entries are unnecessary.
4. `moveRow` moves an item to another list in one commit; an offline edit follows it.
5. Revoking a recorded device rotates only scopes it could read, and queues new
   epochs for remaining readers. No replicated row records recipients.
6. A horizon declares per table and list, with participants from the issuer
   registry. Finalize items, then categories, then lists. Custody relays the
   statements and acknowledgments; a participant need not contact an issuer.

Shopping version 6 exposes only lists, categories, items, requests and decisions
on members. Device names and kinds are trusted-only; command identities in
request/decision rows appear as **This device** or **Another device** on members.
Local contact labels never replicate. A member's directory holds no other
device's node key; saved contacts and proofs keep
[one-way references](../decisions/0062-node-references-and-credential-aliases.md).

`./examples/shoppinglist-app/qualify` runs exactly this with each device in its
own process; the [shopping qualification](shopping-qualification.md) records
what is proven.

## Limits

A device keeps strong command payloads behind a result window (256 by
default) and prunes older ones once required readers applied them
([strong history](strong-history.md)). Effect and recovery artifacts still
share a fixed 4,096 records and 32 MiB that is not pruned, and each receipt
rewrites the device's coordinator evidence file. Plan strong commands for
low-volume transitions; see the [measured limits](end-to-end-evidence.md#device-limits).

Qualified on JVM desktop, macOS arm64 and Linux x86_64, loopback only.
[Android](android.md) has one emulator and one LAN phone run; the two-phone
shopping run is [outstanding](shopping-qualification.md#outstanding). No Internet
relay/NAT, iOS, web or cross-provider migration claim.
