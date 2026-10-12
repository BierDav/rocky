# Running an always-on custody peer

The `rocky-peer` launcher (`rocky_sdk::CustodyPeer`, `crates/rocky-sdk/src/custody.rs`)
runs a configuration-only custody node.

A custody peer holds encrypted frames without reader keys, row projections,
command execution or issuing authority. It uses the same authenticated secure
runtime as devices; it verifies signatures and the author's scope write grant
before receipt, and grants recipients only authorized ciphertext. Its receipt
does not count as applied full-node custody for pruning or eviction.
A custody grant covers every partition of its tables. A device
offers each list's partition tuple to a peer whose credentials grant custody
without read, and the peer registers and keeps it across restarts
([decision 0045](../decisions/0045-custody-partition-offers.md)). It does not
admit horizon statements or forward acknowledgments: participants exchange them
with an issuer directly.

## Enroll and configure

An issuer creates an invitation with
`{"type":"pairing","profile":{"kind":"custody","scopes":["items"]}}`.
Kotlin uses `GrantProfile.Custody(setOf("items"))`. Deliver the invitation
privately to the operator; it expires after ten minutes and enrolls one device.
The issuer must be reachable for this first enrollment.

Mount this configuration as `/config/peer.json`, and persistent storage at
`/data`. `schema` is the same SDK schema object the devices use; no Rust source,
compiled reducer or coordinator is needed.

```json
{
  "directory": "/data",
  "schema": <the application's schema JSON, e.g. Kotlin `ShoppingSchema.json()`>,
  "transport": {"kind":"tcp","listen":"0.0.0.0:7447",
    "advertised":"peer.example.net:7447"},
  "invitation": "rocky://join/<issuer invitation>",
  "peers": ["rocky://peer/<device contact>"],
  "enrollment_upstream": "rocky://peer/<separately authorized issuer contact>"
}
```

Run `rocky-peer /config/peer.json`, or build and run the container:

```sh
docker build -f crates/rocky-sdk/Dockerfile -t rocky-peer .
docker run --name rocky-peer --restart unless-stopped -p 7447:7447 \
  -v "$PWD/peer.json:/config/peer.json:ro" -v rocky-peer-data:/data \
  rocky-peer /config/peer.json
```

The process prints its public contact URI once, then serves until SIGTERM or
SIGINT drains networking and flushes storage. Add its contact on each device,
and each device's contact to `peers`; contacts grant no authority, and the peer
serves any device whose chain and the revocation set admit it. The SDK keeps
64 contacts and dials 16 per pass. TCP needs an explicit listening address. iroh is also
supported through the [starter options](sdk-transports.md#iroh); use a
fixed UDP bind and published route or relay/discovery as appropriate.

On restart, the existing custody identity resumes without using the invitation.
Remove the consumed invitation from the configuration. A reader, writer or
issuer identity, non-custody grants or recorded reader keys are refused before
opening the runtime. Configuration names no schema creation or issuing mode.
Credentials still expire: an issuer must be reachable to renew them.

The [release workflow](../releasing.md) builds amd64 and arm64 on native runners
and publishes `ghcr.io/bierdav/rocky-db-peer` with the version, channel and
`sha-<commit>` tags. Pin the manifest digest and handle GHCR visibility as for
the [coordinator image](coordinator-operations.md#container-image-and-deployment-shape).

## Change-driven sync

The peer enables one sync worker for its network service. Kotlin enables it
with `automaticSync = true` or `syncContinuously()`; the latter observes native
status and has no interval parameter. Cancelling that observer leaves the
service's worker running until disabled or detached. Rust/JSON clients enable
it with `{"type":"sync_start","enabled":true}` and disable it with `false`.
`sync_next` waits for a changed status token; `network.sync` shows the last pass.
Explicit `sync` remains a single pass with its existing response shape.

Committed local writes, new incoming ciphertext (including keyless custody),
peer configuration, network attachment and connectivity changes wake the worker.
Duplicate incoming frames do not. iroh watches endpoint address changes; other
adapters may supply `Transport::connectivity`. On TCP or a foreign adapter,
call `setOnline(true)` / `{"type":"connection","online":true}` when the
application learns connectivity returned. Offline mode suspends attempts.

With the worker enabled, admitted peers keep an authenticated
[live link](live-links.md). A committed frame is pushed over it after flush;
a small edit needs one message and no receiving reconcile. Overflow and
non-frame changes reconcile on request channels of the same link. A new link
reconciles before steady live traffic. Only a busy refusal retains a native
sync fallback. Explicit sync and a disabled remote worker use short links,
which are not repeatedly reopened at idle. Links close on authority changes,
offline mode, disabling the worker and detach. Idle persistent links run only
their 15-second ping.

A pass reconciles at most four peers concurrently. Each peer has its own
back-off: 1, 2, 4, 8, 16, 32 and then 60 seconds. A local write wakes the
worker but leaves a failed peer's deadline intact. Connectivity and
`setOnline(true)` reset all peers; a peer configuration change or an
authenticated inbound session or verified renewal response resets that peer alone. Reachability is recorded
as each peer answers; the pass status follows once all dialed peers finish.

Data-pass retry timers run only for pending changes, requested metadata refresh
or bounded continuation; an idle failed data peer waits for a signal, as does a
successful idle peer. Link reconnection has a separate back-off deadline, so a
busy link slot does not postpone a ready data pass. A queued commit wakeup still
runs the first pass to an unattempted peer.
Disabling with `sync_start` cancels the active automatic pass and acknowledges
its cancellation within one second. Committed frames stay whole and the next
pass repeats interrupted work. Enabling starts a pass immediately, resetting
back-off. Cancelling a Kotlin status observer alone does not disable the worker.
Tokio's controllable clock qualifies the schedule. Committed observation and sync notifications never
substitute for the runtime's durable export fence. Ordinary row and readiness
observers also wait on events; credential expiry uses a single deadline.

Command-result fetch failures and bounded continuation retain retry work without
blocking row sync. An external coordinator head change is not a connectivity
signal: applications that learn of it should request sync. No new coordinator
watch protocol or Internet/NAT qualification is claimed here.

The paused-clock scheduling, cancellation and idle evidence is recorded in
[phase 2](../features_07/02-change-driven-sync.md#measured-evidence-2026-10-10).

## Backup and replacement

Keep `/data` on durable storage; an ephemeral container layer loses custody.
The file provider keeps only this peer's user, node and transport secrets in
owner-only files. Protect those secrets even though there are no reader keys.

Stop the process gracefully, then copy **the entire data directory**, including
`identity.json`, `secrets/` and SQLite files. Do not copy a live SQLite file
alone. The Rust `CustodyPeer::backup` API also produces the SDK's consistent
[backup archive](backup-and-restore.md); the configuration test exercises it.

Restore the stopped directory to a replacement and start the same configuration.
Never run two processes with the same signing identity. If keys are lost, enroll
a fresh custody peer, exchange contacts, and sync it from devices that retain
the history. A keyless peer cannot create a plaintext checkpoint: retain enough
readable replicas to recover pruned data. Do not delete the old copy until the
replacement's receipt frontiers cover the retained data.

## Issuer availability

Optional `enrollment_upstream` configures one explicit issuer contact on the
custody-only node. Omitting it disables forwarding, including on configuration
restart. JSON clients set or clear it with
`{"type":"enrollment_upstream","contact":"rocky://peer/…"}` or a null contact;
clear it before removing that issuer peer. No issuer is selected automatically.

An issuer first creates the invitation. Rust `route_enrollment(uri, contact)`,
Kotlin `session.routeEnrollment(uri, custodyContact)` or JSON
`{"type":"enrollment_route","uri":"…","contact":"rocky://peer/…"}` replaces
its routes with the custody contact. Join with the returned invitation. To
route existing renewal, apply the same function to an issuer contact and add
the returned contact on the device. Add ordinary custody/device contacts on
both sides separately for row sync: enrollment routes never become sync routes.

The custody node forwards pairing/renewal bytes to the configured issuer;
the original issuer and caller still verify requests and answers. Tokens,
issuer identity, expiry and grants remain unchanged. Forwarding survives node
restart, but requires that separately authorized issuer to be reachable. It
cannot mint an invitation, sign credentials or fail over to another upstream.
When all issuers are offline, renewal reports retryable `unreachable`; existing
invitation expiry and refusal rules still apply.

Routed invitations use version 4 and enrollment-only contacts version 2; see
[persisted formats](sdk-enrollment.md#persisted-formats).

Custody grants confer no publisher key, so this peer cannot pair a new device or
sign renewed credentials while all issuers are offline. Requests are refused,
never reported as successful. Existing devices still exchange encrypted rows
through it while their credentials are valid. No issuer key is silently
installed in a custody peer; see [issuer recovery](sdk-enrollment.md#issuer-recovery).
