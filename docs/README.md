# Documentation

Start new work with [architecture V4](proposals/architecture-proposal-v4.md) and
[features_07](features_07/README.md). They define the accepted modular direction,
no-Rust consumer principle and bounded public coordinator. Runtime guides describe
implemented behavior; proposed capabilities are not implied by current API names.
Select the guide for your task rather than reading every document.

| Task | Read |
| --- | --- |
| Build a Kotlin app on a coordination service, from schema to custody | [Getting started](runtime/getting-started.md) |
| Inspect phase-9 end-to-end and soak evidence | [End-to-end evidence](runtime/end-to-end-evidence.md) |
| Upgrade schema/handlers or resume a projection plan | [Generations](runtime/generations.md) |
| Back up a database and restore under a fresh signer | [Backup/restore](runtime/backup-and-restore.md) |
| Compose independent Kotlin APIs or consume packaged artifacts | [SDK packaging](runtime/sdk-packaging.md) |
| Package for iOS and follow application lifecycle | [iOS](runtime/ios.md) |
| Inspect phase-8 execution/platform evidence | [Recovery/SDK evidence](runtime/sdk-packaging.md#build-and-consume-a-package) |
| Set up tooling or run checks | [Development](development.md) |
| Write commit messages | [Commit messages](commits.md) |
| Format code or set up lint gates | [Linting](linting.md) |
| Publish a nightly, alpha, beta, rc or stable release | [Releasing](releasing.md) |
| Locate the owning module | [Module boundaries](modules.md) |
| Use the pure operation/reducer APIs | [Implemented semantics](applications/todo/operation-semantics.md) |
| Install shared-file domains, strong transitions or typed reads | [App-owned runtime](runtime/app-runtime.md) |
| Page through large results and choose a cursor kind | [Paged reads](runtime/paged-reads.md) |
| Inspect app-runtime qualification and measured limits | [Adapter evidence](runtime/app-runtime.md#examples-and-qualification) |
| Implement an app or recover accepted work | [Supported runtime API](runtime/supported-api.md) |
| Reopen, inspect compatibility and understand rejection | [Audit recovery](runtime/local-replica.md#audit-and-offline-verification) |
| Run the persistent desktop demo or use runtime APIs | [Local replica](runtime/local-replica.md) |
| Use two-replica transport, progress and recovery | [Two-replica sync](runtime/local-replica.md#replication-port) |
| Publish immutable snapshots and recover strong commands | [Strong publication](runtime/strong-publication.md) |
| Use signed grants, encrypted rows and authenticated sync | [Permissions and sync](runtime/permissions-and-sync.md) |
| Run encrypted commands or the employees desktop demo | [Encrypted commands](runtime/encrypted-commands.md) |
| Recover commands after coordinator retirement | [Coordinator retention](runtime/coordinator-retention.md) |
| Register isolated domains on a public or self-hosted service | [Public coordination](runtime/public-coordination.md) |
| Provision, monitor, back up or restore public voters | [Coordinator operations](runtime/coordinator-operations.md) |
| Build the voter image or connect an application to a deployed service | [Coordinator deployment](runtime/coordinator-operations.md#container-image-and-deployment-shape) |
| Inspect public-service isolation, fairness and recovery evidence | [Public coordination qualification](runtime/coordinator-evidence.md#public-service) |
| Bootstrap a mesh or operate persistent shared voters | [Shared coordination](runtime/shared-coordination.md) |
| Recover encrypted replicas or run durable effect jobs | [Recovery and effects](runtime/recovery-and-effects.md) |
| Bound encrypted history access, prune redundant archives or rebuild projections | [Scaling and rebuilds](runtime/scaling-and-rebuilds.md) |
| Use the language-neutral SDK or Kotlin suspend/Flow facade | [SDK contract](runtime/sdk-contract.md) |
| Generate a typed schema from Room, install it and write rows by business key | [Logical schemas and row identity](runtime/logical-schema.md) |
| Define a schema without Room or Rust and check what each profile supports | [Schema definitions](runtime/schema-definitions.md) |
| Run SQL statements and transactions on typed tables and recover a commit | [SQL sessions](runtime/sql-sessions.md) |
| Read through snapshot cursors, list a session's catalog or observe committed tables | [Queries and observation](runtime/queries-and-observation.md) |
| Bind tenants or several domains to one SQL session, or move a domain between files | [Database bindings](runtime/database-bindings.md) |
| Commit encrypted rows of several scopes atomically or bind a session to its authority | [Secure sessions](runtime/secure-sessions.md) |
| Run SQL on typed tables of an encrypted replica | [Secure SQL](runtime/secure-sql.md) |
| Read confirmed command rows as read-only tables | [Confirmed tables](runtime/confirmed-tables.md) |
| Integrate Room, pair desktop apps or review the shopping-list developer API | [Room on Rocky's file](runtime/room-integration.md) |
| Enroll, renew, revoke or log out SDK devices and choose where keys live | [SDK enrollment](runtime/sdk-enrollment.md) |
| Join from a checkpoint, catch up after pruning and configure SDK retention | [SDK bootstrapping](runtime/sdk-bootstrapping.md) |
| Configure iroh for SDK devices, follow link liveness and per-path round trips | [SDK transports](runtime/sdk-transports.md) |
| Serve Rocky on your own iroh endpoint next to your protocols, or use the starter | [iroh networking](runtime/sdk-transports.md#iroh) |
| Find domain members on the local network or through the topic, and read the status of each path | [Discovery](runtime/discovery.md) |
| Open a secure database without a network, share it between sessions or attach your own transport | [SDK composition](runtime/sdk-composition.md) |
| Inspect scaling/observer measurements and Kotlin qualification limits | [Phase 6 evidence](runtime/scaling-limits.md#measured-limits) |
| Run sustained scale qualification and collect hardware evidence | [Scale qualification](runtime/scaling-limits.md#scale-qualification) |
| Configure retention, sync a peer behind the horizon, rebootstrap or audit a file | [Checkpoints, pruning and rebootstrap](runtime/checkpoints.md) |
| Find why pruning stopped and release a strong request or silent peer | [Retention holds](runtime/retention-holds.md) |
| The same for `EncryptedReplica` | [Encrypted checkpoints](runtime/encrypted-checkpoints.md) |
| Import a checkpoint page by page and read rows while it runs | [Importing an encrypted checkpoint](runtime/checkpoint-import.md) |
| Run full replica nodes, subscribe a device to some scopes, evict one or read remotely | [Full and partial replicas](runtime/replica-roles.md) |
| Find throughput, read and retention limits and the measured options | [Scaling limits](runtime/scaling-limits.md) |
| Group writes, receive pages, wait for queue space and read writer counters | [Write concurrency](runtime/write-concurrency.md) |
| Measure what a local change costs until its observer shows it | [Local latency](runtime/local-latency.md) |
| See what the earlier engine cost at 5,000,000 rows and 2,000 tables | [Engine at target size](../benchmarks/engine-scale/RESULTS.md) |
| Inspect coordinator storage plateau and expiry evidence | [Retention qualification](runtime/coordinator-evidence.md#retention-probe) |
| Inspect three-process coordination and crash evidence | [Coordination qualification](runtime/coordinator-evidence.md#coordination-and-crash-boundaries) |
| Register identities, delegate Directory rights or delete a user | [Directory registration](applications/directory/registration.md) |
| Write or reorganize documentation | [Documentation conventions](documentation.md) |
| Understand the target product and module boundaries | [Architecture V4](proposals/architecture-proposal-v4.md) |
| Select the next agent implementation task | [Current phases](features_07/README.md) |
| Read the agreed design behind the current phases | [Mesh](features_07/scope-mesh.md), [iOS](features_07/scope-ios.md) |
| Understand engine selection, buffered commits and the file layout | [Storage engine and durability](runtime/write-concurrency.md#design-rationale) |
| Record a new architectural decision | [Decision records](decisions/README.md) |
| Find optimizations deferred until a workload needs them | [Optimizations on demand](optimizations_on_demand/README.md) |
| Find known defects that are not scheduled | [Fix later](fix_later/README.md) |

Implemented guides live under `runtime/`, wire/behavior contracts under
`protocol/`, and application-specific guidance under `applications/`.

## Status of the proposals

[V4](proposals/architecture-proposal-v4.md) is the accepted architectural direction.
[V3](proposals/architecture-proposal-v3.md), [V2](proposals/architecture-proposal-v2.md)
and the [original proposal](proposals/architecture-proposal.md) are historical.
Room-specific compatibility work is deferred. Existing decision records govern
implemented formats until a phase explicitly replaces them with a versioned design.

V4 and its focused phase briefs replace the old long proposal as implementation
navigation. They do not mark scale, security, public-service or platform gates passed.

The proposals exceed the 200-line guideline of the [documentation conventions](documentation.md).
Their length has been considered: retain them as versioned references. New
implementation guidance belongs in focused documents such as the phase briefs.

- [Wide schemas and domain placement](runtime/wide-schemas.md), with [qualification](runtime/scaling-limits.md#measured-limits).
- [Full and partial replicas](runtime/replica-roles.md), with [qualification](runtime/scaling-limits.md#measured-limits).
