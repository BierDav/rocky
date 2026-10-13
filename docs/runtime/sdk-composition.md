# SDK composition: database, sessions and network

`rocky-sdk` composes a secure database, its sessions and an optional network as
separate services. The [Kotlin API and packages](sdk-packaging.md) wrap the same
native handles. Crate direction is in [modules](../modules.md).

## Open a database

A database needs a directory, the application schema and a key store. It needs
no endpoint, no reachable issuer and no coordinator.

```rust
use rocky_sdk::{Database, Open, Opened};

let open = Open { directory, schema, secrets: None, network: None, commands: None };
let Opened { database, .. } = Database::create(open, None)?;
```

| Constructor | Result | Network |
| --- | --- | --- |
| `Database::create` | A new domain; this device is its first issuer | Optional |
| `Database::open` | The domain installed in the directory | Optional; needed by a member whose credentials expired |
| `Database::join` | Enrolls with the issuer an invitation names, as its profile | Required |
| `Database::device`, then `Database::install` | This device's keys; then [supplied credentials](sdk-enrollment.md#supplied-credentials) | Optional |

`commands` composes a coordinator and, on an executing device, a handler for
the schema's [typed commands](typed-commands.md); both stay the application's.
`Coordination::Service` instead names a [coordination service](public-coordination.md)
by trust bundle; the database signs with its node key over its iroh network
and keeps receipt evidence in its directory. Without either, a database still
stores command tickets; nothing runs or settles.

Credentials and keys are never optional. `open` fails when keys are missing,
the schema differs or the device logged out; no key is invented. A local
database still verifies its credential chain and policy at open and on every
session call. Without `secrets`, keys are owner-only files in `secrets/`; see
[secret storage](sdk-enrollment.md#secret-storage).

## Sessions

`database.session()` returns a `Session` carrying the
[native calls](sdk-enrollment.md#native-calls). Up to 64 sessions share a
database, each with its own subscriptions. `session.close()` ends its pending
`next` waits and releases its subscriptions; calls already made finish.

## Attach a network

```rust
use rocky_sdk::Network;

let service = database.attach(Network::Owned(connector)).await?;     // Rocky starts and closes it
let service = database.attach(Network::Borrowed(transport)).await?;  // yours; never closed by Rocky
service.close().await;
```

A network given as `Open::network` starts before the database opens;
`Opened::network` is its service. One network is attached at a time, of any
[transport kind](sdk-transports.md). While attached, the device serves sync to
its [peers](sdk-enrollment.md#peers-and-contacts), an issuer also pairing and
renewal, and `sync`, `pairing`, `contact` and `renew` use it.

## Ownership and closing

| Resource | Owner | At the owner's close |
| --- | --- | --- |
| Database file, directory lock, reactor thread | `Database` | Admitted calls finish; flushed, closed and released |
| Session reader and subscriptions | `Session` | Waits end; other sessions continue |
| Accept loop and inbound streams | `NetworkService` | 5 s drain for admitted work, then cancellation |
| Transport from `Network::Owned` | `NetworkService` | Closed once |
| Transport from `Network::Borrowed` | Application | Never closed by Rocky |
| `SecretStore` and keys | Application or the default file store | Kept; only `logout` deletes keys |
| Coordinator, handler | Application | Never closed by Rocky |

Every close is idempotent and closes only what it owns. `database.close()`
ends its sessions and detaches its network service first. After
`service.close()` the database stays open and the same or another transport
can be attached. A cancelled sync repeats safely.

## Refusals

Combinations that cannot work fail before anything is stored or started.

| `ServiceError` | Code | When |
| --- | --- | --- |
| `NetworkRequired` | `network_required` | `join`, `sync`, `pairing`, `contact` or a member's `renew` without a network |
| `CredentialsExpired` | `network_required` | A member opens with expired credentials and no network |
| `NoRoute` | `rejected` | No route of the attached kind for the peer or in the invitation |
| `NetworkAttached` | `rejected` | A second network is attached |
| `Unreachable` | `unreachable` | Known route attempts failed |
| `TimedOut` | `unreachable` | A route attempt timed out |
| `Closed` | `closed` | A call on a closed session, database or network service |

`Unreachable` and `TimedOut` are retryable as the same call.

## Network status

Native `network()` returns `NetworkStatus`, also after logout. It exposes
attachment lifecycle and optional ownership, explicit networking/foreground
controls, optional inbound service counters, discovery, live peer links,
relay observations and retained dated attempts. An absent service has no
invented zero counters. Peer history does not become a `connected` or
`unreachable` state; only an open authenticated link counts as connected.

`DatabaseStatus` adds independent authority and data observations, and
`syncOnce()` returns a per-peer `ReconciliationReport`. See the
[observable status contract](../proposals/observable-status.md) for semantics,
unknown observations and generated Kotlin parity.

## The starter engine

`Engine::open` and `NativeSession.open` take JSON options with `mode`
`create`, `join` or `resume` and compose one database, one session and one
owned network, adding no behaviour. `Engine::open_with` takes a `Network`
instead; a borrowed one stays open. A `coordinator` option holding a trust
bundle composes that service; it cannot be combined with composed `commands`.

## Native handles

`NativeDatabase` owns the file and reactor; `NativeDatabaseSession` closes only
its subscriptions and waits; `NativeNetworkService` closes only an attachment.
UniFFI names async shutdown `shutdown` to avoid generated Kotlin
`AutoCloseable.close`. Kotlin wraps them as `RockyDatabase`, `RockySession` and
`RockyNetworkService` with suspend `close`.

## Qualification

`cargo test -p rocky-sdk --test composition`: a database created, read,
written and reopened with no network, and enrolled devices reopening with no
further transport start or dial; sync on databases attached later; two
sessions where closing one leaves the other delivering; a borrowed transport
surviving its service and database and serving again; an owned transport
closed once; a never-completing dial cancelled at the drain bound with the
database usable and a later sync converging. Loopback only.

## Design rationale

- Database, session and network are separate owners so that a database opens
  without a transport and closing it never closes application infrastructure.
- Reference-counted database handles were rejected: an explicit owner gives
  close a definite meaning.
- A given transport starts before the replica, so a failed transport leaves no
  half-created domain; rolling back a created domain was rejected.
- The SDK exports no SQLite connection or writer; persisted or transmitted
  data crosses boundaries as versioned bytes or descriptors.
