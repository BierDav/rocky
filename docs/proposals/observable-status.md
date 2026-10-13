# Observable database and network status

The native SDK defines the current status contract in
[`rocky-sdk::status`](../../crates/rocky-sdk/src/status.rs). Generated UniFFI
records and enums expose the same fields, variants and nullability to Kotlin.
Kotlin adds suspend/Flow calling conventions, without a second status model.

## Independent facts

Rocky is a [local-first composition](architecture-proposal-v4.md). Authority,
local data, replication and strong commands have different dependencies.
`DatabaseStatus` groups observations without a global connection or readiness
verdict:

| Group | Observations |
| --- | --- |
| `authority` | Identity, credential expiry/logout, verified revocation evidence, grant profile and known renewal routes |
| `data` | Committed revision, generation manifests/rebuild progress, bootstrap counts, scope import checkpoints/cursors, held key epochs and ticket counts |
| `network` | Attachment lifecycle/ownership, networking and foreground controls, worker activity, discovery, peer links, attempts, relay observations and last reconciliation |
| `coordination` | Configuration mode and coordinator operation observations |

`NetworkingDisabled` means the application disabled networking. Suspension is
separate, and a worker can stay enabled while suspended. Networking can be
enabled with no attachment. `setNetworkingEnabled` and `setAutomaticSync`
change controls; observing status enables neither.

`status()` reads the current observations. `statusAfter(token)` waits for a
change, and Kotlin `observeStatus()` adapts that wait to a cancellable Flow.
Reads do not dial, resolve contacts or probe services. Credential expiry wakes
observers without requiring a sync. Tokens exclude path measurement counters;
explicit snapshots refresh those measurements. Snapshots combine local reads,
not an atomic distributed cut.

## Links and attempts

`connectedPeerCount` counts the exposed authenticated links whose state is
`Open`. Inbound links count too. `Opening`, `Closing` and `Closed` describe the
link resource lifecycle. An open link remains subject to transport failure
detection delay. A completed short exchange can leave no open link.

Each attempt records a stable runtime ID, target, operation, stage, elapsed
time, optional wall-clock timestamps and outcome. A missing outcome denotes
an active attempt. Outcomes are `Completed`, `NoResult`, `Cancelled` and
`Failed`; failure kinds distinguish timeout, transport, remote refusal, local
authorization, protocol, coordination and unknown errors. Diagnostic strings
are retained without using their wording to infer revocation or reachability.
Unknown wall-clock timestamps remain null.

History keeps at most 512 completed/active observations when completed entries
can be evicted; active attempts are protected. It resets on reopen and is not
a durable audit. Peer attempts, contact-resolution attempts and coordinator
attempts remain separate. Known routes are configuration, not contact proof.
The authenticated-exchange timestamp does not imply an inbound direction.

A success proves that operation happened. It does not establish future
reachability, global freshness, remote custody or command eligibility. A
failure after admission can coexist with an open link. No peer links says
nothing about whether the device has internet access.

## Coordinator and relay boundaries

Coordination modes distinguish no coordinator, an application-composed port
and a certified service. Observations name actual coordinator API operations,
not a fabricated persistent connection. The logical `coordinator` target
identifies the configured port; it does not expose a physical voter socket.
A quota refusal records a coordination failure, including its supplied code.
It does not imply the service could not be contacted.

Iroh reports its actual home-relay watcher entries: relay URL, connection flag,
last error and authorization-denial reason. Connectivity notifications include
relay transitions and endpoint address updates; there is no periodic health
probe. Relay entries are null when the adapter cannot observe them, and an
empty list means the watcher currently has no entries. Initial relay
configuration is known for a Rocky-created endpoint; it is null for a borrowed
endpoint. Rocky does not infer the original configuration from its address.

Live paths retain direct/relay selection and transport measurements. A direct
peer connection can survive relay loss. Relay attachment does not prove a
particular peer answers, and discovery rendezvous has its own observations.
Application-owned endpoints remain application-owned on Rocky shutdown.

## Authority and data

Verified signed revocation is an authority fact with its evidence identity and
statement. It survives reopen and ends admission. A diagnostic containing
“revoked” is not that evidence. Expiry, logout and revocation remain independent.
Locally valid credentials cannot establish that no unseen revocation exists.

Bootstrap exposes counts and pending intent, including before totals are known.
Per-scope checkpoint/cursor observations remain visible independently of worker
activity. Import page completion is null where no page-completion fact is
available. Generation rebuild and held key epochs retain their identities.
No universal `Ready`, `canWrite` or freshness boolean is inferred from these
facts; operation-time admission remains authoritative.

## Reconciliation reports

`syncOnce()` returns `ReconciliationReport`. It retains the finite selected
peer set, per-peer skip reasons, actual attempts, known completed transfers,
refused/waiting scopes, result retrieval failures and retry intentions. No
selected peers produces an empty report. Networking disabled and suspension
produce explicit skips. Backoff is a skip, not a failed dial. A returned
report means the pass ran, not that any peer was reached: a pass whose every
attempt failed, or whose every peer was skipped, still returns its report.
Read success from the attempts.

Transfer `pulled`/`pushed` counts describe operation frames, not projection rows.
`bytes` counts encoded received frames, including duplicates; it excludes
checkpoint pages, pushed frames and protocol overhead. Transport traffic
counters remain on live paths.

Partial peer failure retains other peers' successful work and completed scope
receipts already captured before the failure. An interrupted scope without a
receipt cannot claim a completed transfer; committed revisions and import
progress remain observable separately. Cancellation propagates, while the
attempt history records cancellation. Local precondition failures remain typed
errors carrying `code`, `retryable` and `detail`.

Applications derive labels such as “Networking disabled”, “2 peer links”,
“Credentials expired” or “Importing 42%” from these facts. Percentages and
prioritized badge labels belong to applications. They are not additional SDK
states. Activity is a bounded diagnostic feed, not the source of status facts.

The replacement has one current API contract, with no old connection/readiness
enums or compatibility decoding. See [composition](../runtime/sdk-composition.md),
[bootstrap](../runtime/sdk-bootstrapping.md#readiness) and
[live links](../runtime/live-links.md#status-and-lifecycle).
