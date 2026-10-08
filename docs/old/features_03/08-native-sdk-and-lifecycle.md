# 8. Native SQL SDK, configuration and lifecycle

> Superseded implementation brief, retained as an optional Room acceptance
> reference. Do not implement this sequence as the current Rocky roadmap.
> Generic work and deferred adapter work are assigned in
> [features_04](../../features_04/README.md#foundations-and-old-plan-disposition).
> [V4](../../proposals/architecture-proposal-v4.md) governs future design.

Status: superseded future brief; retained optional Room requirements. Owner: `rocky-sdk`, `sdks/native`, ORM-neutral Kotlin core and
platform packaging. Depends on phases 1–7.

## ORM-neutral native boundary

Expose typed session/open, connection, prepare, bind, step/read-batch, reset,
clear-bindings, transaction, outcome lookup, interrupt and close operations.
Opaque handles carry owner/generation checks; stale or cross-session handles fail
explicitly. No raw SQLite pointer, private file path or protocol SQL crosses the
public ABI. Native workers own statement/cursor storage without authored unsafe
code or leaked lifetimes. Extend the safe private adapter rather than exposing it.

Provide all SQLite scalar bindings/getters, column count/names/types, SQL error
codes, row counts and connection-local generated-ID semantics. Structured errors
distinguish definite denial/constraint failure, retryable contention, unavailable
data, incompatible schema and unknown commit outcome. Ordinary Room exceptions
must preserve useful SQLite failure categories; application code never parses
diagnostic strings or acknowledges an unknown outcome as success.

The current UniFFI entry point is asynchronous JSON calls; AndroidX's pinned
driver uses synchronous open/prepare/step methods. Design a synchronous native
command bridge on Room's query execution threads with an independent native
worker. Blocking on work scheduled on the same Kotlin dispatcher risks deadlock.
Do not hold native locks while invoking foreign callbacks or waiting for peers.
Prove scheduling with a one-thread query dispatcher and concurrent sync.

Transport batches values/results to avoid one cross-language call per cell, while
implementing ordinary statement getters over the current authorized batch.
Bound native memory, pending commands and cancellation/interrupt latency.
Cancellation cannot turn an admitted commit into a fresh retry identity.

Native recovery can deduplicate its recorded transaction token; it cannot infer
that a newly invoked DAO method after restart is the same business intent.
Define lost-acknowledgment reconciliation inside the SDK without exposing handles
to repositories. Do not promise exactly-once arbitrary application retries merely
because internal transport retries are idempotent.

## Generated configuration, not repository wiring

One setup descriptor combines generated schema with exceptional policies:
domain binding, atomic placement, replication/read modes, credential/key providers,
update policy, migrations and resource budgets. Ordinary entity access needs no
per-table projection callbacks, durable queue entities or hand-written schemas.

Define these APIs independently of Room; later a driver consumes them. Setup
can select a trusted development or authenticated profile, with distinct types or
unambiguous configuration. A default database filename is a logical instance name,
not authorization to open/modify that physical file directly.

The SDK owns enrollment, pairing, reconnect, background replication, subscription
changes, full-node selection, renewal and status. Status includes held scopes,
readiness, pending/unknown outcomes, durable watermark/custody and strong/effect
progress without adding Rocky fields to every Room entity. Business repositories
do not drain queues or refresh read models. Recovery tools may expose explicit
administrative controls outside those repositories.

## Lifecycle

One owner coordinates native runtime and SQL connections. Define ownership when
multiple Room instances share one runtime: closing a connection does not shut
down another instance's session; final owner close drains admitted work, releases
leases/cursors and stops subscriptions/sync. Close is idempotent and cannot wait
forever behind a foreign transaction whose owner disappeared.

Different principals/domain bindings get separate sessions and cache namespaces.
Logout stops new admission, rolls back active unauthorized leases, drops
undelivered batches and closes observation before key/cache eviction. Credential
renewal does not silently replace authority inside a transaction.

Process restart rolls back uncommitted SQL, restores durable outcomes and resumes
replication under the recorded identity. OS locking prevents two writers of the
same file. Define multiprocess access deliberately; do not advertise Android
Room multi-instance invalidation without a native ownership/event design.

## Platform and packaging gates

JVM desktop remains the first integration target. Package generated bindings and
prebuilt native artifacts so consumers need no Cargo, Rust, host executable path
or Python. Verify supported CPU/OS combinations rather than assuming a successful
macOS build proves Linux/Windows or Android support.

Android additionally needs packaged ABIs, min-SDK/driver compatibility, Keystore
integration, app background/foreground lifecycle, process-death recovery and
database-directory ownership. iOS needs native driver bindings, Keychain,
coroutine/Swift lifecycle and suspension handling. Do not use desktop JNA paths
as a mobile design. Web/asynchronous driver APIs are a separate target.

Mobile qualification is required before an AndroidX Room Android claim; it need
not block developing the JVM adapter after its native gate. The release matrix
must expose this distinction. A generic Room claim cannot imply every platform.

## Completion gate

- A native SQL consumer exercises schema, scalar values, transactions, cursors,
  errors, recovered commit outcomes and logical invalidation without Room.
- Pinned Kotlin/UniFFI bindings pass synchronous-driver scheduling, interrupted
  calls, single-thread dispatcher, stale-handle and owner-close tests.
- Generated configuration removes schema/projection duplication and isolates
  principals/domains with no repository lifecycle code.
- Runtime close, logout and renewal have bounded, tested cleanup; restart and
  reconnect retain durable transaction identities and installed bindings.
- Consumer artifacts contain native resources and pass clean-environment launch;
  publication/platform limitations are explicit until qualified.

Run native/core SDK checks, targeted Rust tests and `./scripts/check`. Platform
tests run separately; Rust workspace success is not Android/iOS evidence.
