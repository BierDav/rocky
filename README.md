# Rocky DB

Rocky DB is being aligned into a composable P2P backend toolkit: local-first
SQLite data/replication, credentials, explicit typed strong commands and optional
iroh plumbing, with a provider-operated public metadata coordinator for startup
without an application backend. Schemas, handlers and payloads stay on app nodes.

The accepted [architecture V4](docs/proposals/architecture-proposal-v4.md) and
[agent-ready implementation phases](docs/features_07/README.md) are the current
path forward. Every capability has a language-neutral integration contract:
consumers should define schemas and handlers in their own language and use their
chosen ORM/transport adapters without a Rust project or code. Kotlin/JVM desktop
is the first complete consumer target; universal adapter availability is not claimed.

Phases 1 to 8 of an earlier roadmap are implemented; the current one has not
started. [Recoverable generations](docs/runtime/generations.md),
[fresh-device backup/restore](docs/runtime/backup-and-restore.md) and
[composable packaged Kotlin SDKs](docs/runtime/sdk-packaging.md) provide the bounded
single-file/JVM desktop profile; [qualification limits](docs/runtime/sdk-packaging.md#build-and-consume-a-package)
remain explicit.
[Typed strong commands](docs/runtime/typed-commands.md) have host-language
handlers and durable tickets, with development and registered service coordinators;
the persistent coordinator now has [bounded retention](docs/runtime/coordinator-retention.md)
with explicit retry/expiry outcomes and app-held historical evidence. Public
registration and iroh client access are implemented in the
[public service software](docs/runtime/public-coordination.md), with
[operator runbooks](docs/runtime/coordinator-operations.md) and
[qualification evidence](docs/runtime/coordinator-evidence.md#public-service). No live
public service or universal ORM/platform support is claimed.
The phase 9 [end-to-end qualification](docs/runtime/end-to-end-evidence.md) runs a
Kotlin-only application against provisioned voters on loopback. Its acceptance
scenarios pass; [bounded encrypted strong history](docs/runtime/strong-history.md) addresses
the retained-command budget and reopen audit growth. The missing real-network
run and qualification boundaries remain recorded there.
The existing implementation and qualification limits are described below.

The Laptop and Phone demo supports persistent create/edit/complete/delete and
offline convergence over a controllable in-process connection. Strong snapshot
publication uses a trusted in-memory coordinator with executor
fencing, sealed-input replay and interrupted-request recovery. See
[strong publication](docs/runtime/strong-publication.md) for APIs and ledger-loss limits.
See [two-replica sync](docs/runtime/local-replica.md#replication-port) for the
transport and recovery contract, and the [local replica guide](docs/runtime/local-replica.md)
for storage APIs and qualification.

Manifest-bound app runtimes support independent domains spread over one or more
SQLite files, with buffered commits flushed before anything leaves the device,
general strong transitions with job intents, authorized bound/typed reads and
payload-bound strong reads. The [inventory example and adapter evidence](docs/runtime/app-runtime.md)
cover this boundary and its scheduling limits.

[Wide schemas and explicit placement](docs/runtime/wide-schemas.md) support
resumable schema bootstrap, selected files and additive non-unique indexes.

[Scale qualification](docs/runtime/scaling-limits.md#scale-qualification) now provides a combined
trusted storage soak and hardware evidence capture. All six full-durability
soaks passed at 500,000 live rows. All six phase 5 gates missed throughput and
p99 budgets: mean writes ranged from 2,165 to 5,076/s, slowest intervals from
1,375 to 4,368/s, and p99 from 128.9 to 351 ms. Oracle 1/2/3 reopen exceeded
2 seconds; Oracle 2 reached 327 MiB RSS; Oracle 1's point-read p50 bound was
1.023 ms. Remaining budgets include single-domain install, 5,000,000 live rows,
matched write-cost and observer measurements, 10,000-operation catch-up, faults
under combined topology, strong/effects, encrypted scale and both durability
classes' power-loss evidence. The qualification guide lists every result and
open budget.

A separate [authenticated encrypted row path](docs/runtime/permissions-and-sync.md)
provides signed capabilities, transactional p2panda operation storage and native
TCP sync through a keyless relay, plus encrypted strong commands and scoped
result projection. The [employees desktop demo](docs/runtime/encrypted-commands.md#desktop-demonstration)
uses this path. The legacy todo demo remains trusted.

Root-certified bootstrap, a deterministic Directory app and a separate persistent
OpenRaft service now support shared coordination. See [shared coordination](docs/runtime/shared-coordination.md)
and [three-process qualification](docs/runtime/coordinator-evidence.md#coordination-and-crash-boundaries) for the bounded
native profile and recovery limits. The original todo demo retains its memory coordinator.

[Encrypted recovery and application effects](docs/runtime/recovery-and-effects.md)
support fresh signing identities, explicit acknowledged-loss domains, optional
preconfirmation custody and durable uncertain delivery. Alice and Accounting
demonstrate resource-owned authorization and version-checked review jobs.

[Full and partial replicas](docs/runtime/replica-roles.md) let a device hold a
subset of scopes and rely on full replica nodes for custody, sync by per-log
frontiers and read remotely what it does not hold.

[Bounded history access and shadow rebuilds](docs/runtime/scaling-and-rebuilds.md)
and a [language-neutral SDK with Kotlin/JVM qualification](docs/runtime/sdk-contract.md)
provide initial phase-6 slices. See [evidence and remaining gates](docs/runtime/scaling-limits.md#measured-limits)
for checkpoint, application migration and mobile limits.

[Room on Rocky's file](docs/runtime/room-integration.md) runs Room DAOs on the
replicated tables through Rocky's `androidx.sqlite` driver, with SDK-owned UniFFI
native loading, Kotlin schemas and authenticated enrollment over iroh or TCP.
The shopping-list example has Metro/ViewModel screens, QR pairing, selectable
trusted/full/partial devices and visible [SDK activity](docs/runtime/activity.md).

[Logical schemas and row identity](docs/runtime/logical-schema.md) generate a
typed schema from Room's exported schema, install real tables verified against
it, and replicate rows by application primary key with per-row incarnations.
[SQL sessions](docs/runtime/sql-sessions.md) run ordinary statements,
transactions and savepoints on those tables and commit the changed rows as
operations. [Database bindings](docs/runtime/database-bindings.md) give a
tenant, or several co-located domains, one such session with one atomic
commit. All are Rust APIs on the trusted runtime; the Room driver, the
Kotlin binding and auto-generated keys are not implemented.
On the encrypted path, [secure sessions](docs/runtime/secure-sessions.md)
commit rows of several reader scopes as one signed bundle that every reader
applies atomically, and [secure SQL](docs/runtime/secure-sql.md) runs
authorized transactions on typed tables of an `EncryptedReplica`.

## Getting started

Install Rust through rustup, Jujutsu (`jj`) and ripgrep (`rg`). The repository
toolchain file selects the Rust release and components automatically.

```sh
./scripts/setup   # pinned lint tools, jj fix/push configuration
./scripts/lint
./scripts/check
```

We use **Jujutsu exclusively** for version control, including remote operations
through `jj git`. Do not use the Git CLI in this repository.

Run the macOS app:

```sh
cargo run -p todo-desktop --locked -- --data-dir local/todo-demo
```

## Where to look

| Area | Purpose |
| --- | --- |
| `crates/rocky-core` | Shared protocol types and portable schema definitions |
| `crates/rocky-storage` | SQLite access, row capture, statement analysis, snapshot cursors and table events shared by the runtimes |
| `crates/rocky-runtime` | Trusted transactions, replication and observation |
| `crates/rocky-credentials` | Credential verification, issuance, grant profiles, key providers and enrollment messages |
| `crates/rocky-secure` | Capability enforcement, encrypted replicas, key epochs and native sessions |
| `crates/rocky-iroh` | Optional iroh handlers for an application's endpoint, and the starter's own |
| `crates/rocky-sdk` / `sdks/native` | Generic native SDK lifecycle and UniFFI ABI |
| `crates/rocky-coordinator` | Generic fencing and strong-commit metadata |
| `examples/directory-app` | Identity registration, scoped delegation and deletion disposition |
| `examples/todo-app` | Todo schema, merge rules and command handlers |
| `examples/accounting-app` | Observed event checkboxes and resource-owned review jobs |
| `examples/inventory-app` | Draft notes and strong stock reservation |
| `examples/todo-desktop` | Desktop composition and development controls |
| `sdks/kotlin/rocky-core` / `rocky-room` | ORM-neutral Kotlin APIs and Room projection/action adapter |
| `examples/shoppinglist-app` | Desktop library walkthrough, pairing and Room migration qualification |
| `benchmarks/engine-comparison` | Separate engine qualification workspace |
| `benchmarks/engine-scale` | Separate workspace comparing engines at target size |
| `benchmarks/runtime-scaling` | Scaling probes and timing qualification for the runtime |

Read [module boundaries](docs/modules.md) for ownership and dependencies.
Use [the documentation index](docs/README.md) to select a focused guide.

- [Development workflow](docs/development.md)
- [Commit messages](docs/commits.md) and [releasing](docs/releasing.md)
- [Agent instructions](AGENTS.md)
- [Documentation conventions](docs/documentation.md)
- [Implemented operation semantics](docs/applications/todo/operation-semantics.md)
- [Current implementation roadmap](docs/features_07/README.md)
- [Current architecture proposal](docs/proposals/architecture-proposal-v4.md)
- [Engine decision](docs/runtime/write-concurrency.md#engine)
