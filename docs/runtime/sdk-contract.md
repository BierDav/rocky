# Language-neutral SDK contract v1

The version-1 JSON call contract of the trusted `rocky_runtime::sdk::Session`
facade. The encrypted SDK's [native calls](sdk-enrollment.md#native-calls) use
the same envelope; the [Kotlin SDK](sdk-packaging.md) is the packaged consumer.

## Ownership and wire encoding

The host supplies an already authorized `Replica<A>` to `sdk::Session::new`.
The session cannot open files, change policy, install executable code or run SQL
writes. It binds every call to exactly one domain. This facade currently covers
the trusted compiled `StoredApplication` runtime; it does not replace the signed
`EncryptedReplica` authentication/transport boundary.

A JSON call contains `version: 1`, the complete `scope` and a tagged `request`.
Scope contains `mesh`, `app` and `domain`, each an array of sixteen unsigned bytes.
Handles/commitments are arrays of 32 unsigned bytes. Operation/request references
contain a sixteen-byte `node` and decimal-string `sequence`. All revisions,
sequences and SQL INTEGER values use decimal strings, preserving signed 64-bit
precision across languages. Bound values are tagged `null`, `integer`, `real`,
`text` or `blob`; blobs use arrays of unsigned bytes. A `real` is the 16
lowercase hexadecimal digits of the double's bits; NaN and negative zero are
rejected. See [scalars](logical-schema.md#scalars).

Replies contain `version: 1` and either `status: "ok", value: {...}` or
`status: "error", error: {code, retryable, message}`. Codes are stable strings:
`denied`, `unsupported_generation`, `missing_dependencies`, `missing_head`,
`quota`, `corruption`, `conflict`, `closed`, `unknown_confirmation` and `storage`.
Messages are diagnostic. Use codes/retryability, not message parsing. Unknown
versions fail; malformed calls and unknown request fields fail explicitly.

## Calls and progress

| Request tag | Inputs | Result |
| --- | --- | --- |
| `read` | `query` | Applied local snapshot with completeness |
| `read_strong` | `query` | `ready` snapshot, `pending` discovered head/reason, or `unavailable` |
| `subscribe` | `query` | Session-local decimal-string subscription token |
| `next` | `subscription` | Latest committed snapshot; waits after initial delivery |
| `unsubscribe` | `subscription` | Idempotent release and cancellation of a pending `next` |
| `mutate` | `handle`, nullable `entity`, precise `payload` bytes, nullable decimal `baseline` | Locally committed operation reference; may still be buffered |
| `mutation_progress` | `handle` | `committed` operation or `unresolved` |
| `invoke` | `handle`, `method`, `version`, `arguments` bytes, `policy` commitment | Durably queued strong request reference |
| `command_progress` | `handle` | Request, explicit command state and nullable outcome bytes |

Queries contain `sql`, tagged bound `values`, `max_rows` and optionally
`max_bytes` and `cursor`. A snapshot holds one page; its nullable `next` is the
`cursor` of the following page of the same query, and `next_kind` is `keyset`
or `offset`. An offset cursor costs more the deeper it is and shifts under
concurrent writes; [paged reads](paged-reads.md) lists the query shapes that
get a keyset cursor. Reads evaluate only
[application-authorized views](app-runtime.md#read-authorized-local-data) over
stored rows. Protocol tables and unrestricted writes remain inaccessible. The caller's scope must
match the session. A local read never implies coordinator completeness.
`read_strong` requires the host's coordinator and matching retained result payloads.
Missing coordinator configuration is an explicit `missing_head` error.

Mutations contain app-defined precise actions, not arbitrary full-row ORM changes.
A baseline mismatch rejects the mutation. A committed mutation is buffered by
default: power loss can drop it until the runtime flushes, which it does before
the operation leaves the device
([design rationale](write-concurrency.md#durability-and-exposure)).
`mutation_progress` then reports `unresolved` and resubmitting the handle commits
it once. Reusing a committed handle with identical
input returns its established operation even after the revision advances;
conflicting input fails. `invoke` queues a request; scheduling belongs to the host.
Command states are `unresolved`, `queued`, `prepared`, `outcome_unknown`,
`confirmed_locally`, `peer_applied` and `released`; a released request no longer
holds checkpoints and is not executed by this replica
([retention holds](retention-holds.md#an-unresolved-strong-request)). A local confirmation does not prove remote
custody, remote application or external-effect delivery.

An unresolved handle proves no commit evidence was found. It does not prove
admitted work was cancelled or that repeating an external effect is safe.
Recover admitted work through the same stable handle after interrupted waits or
process restart. Encrypted backup/import and effect recovery remain on their
[existing explicit APIs](recovery-and-effects.md).

## Subscription lifetime and cancellation

Registration precedes the initial snapshot, including commits between those steps.
Notifications coalesce; collectors receive the latest committed state, not an event
for every operation. Only one `next` may run per token; another receives retryable
backpressure. Dropping a `next` future abandons its wait without corrupting delivery.
Unsubscribe cancels outstanding waits and releases observer capacity. Session
close releases all subscriptions and rejects later calls, while leaving the host
replica and admitted writes under host ownership.

Kotlin `observe(query)` exposes a cold, conflated Flow and releases its subscription
in `finally`. Registration and cleanup use a noncancellable region so collector
cancellation cannot strand the token. `observe(Flow<Query>)` changes bound queries
with `flatMapLatest`, releasing the previous registration. Suspend-call cancellation
abandons the wait; admitted mutations/requests continue and can be recovered.
A Flow snapshot is a local read; strong reads are explicit suspend calls.
Kotlin client close cancels query, status and activity collectors and waits for
their cleanup before closing its transport. Database close closes its owning
Kotlin session before shutting down the native database.

## Kotlin/JVM fixture

`sdks/kotlin/rocky-core/src/commonMain/kotlin/RockyClient.kt` supplies the suspend/Flow facade.
`sdks/kotlin/qualification/inventory/src/main/kotlin/ProcessTransport.kt` supplies the JVM fixture transport. `InventoryClient` demonstrates typed stock reads and precise note
changes. The fixture Rust host installs Inventory with a trusted caller, executes
its queued commands against a memory coordinator and multiplexes replies by call
ID. It is a qualification/example executable, not a credentialed deployment server
or JNI ABI. It drains admitted work and cancels subscriptions when stdin closes.
The memory coordinator does not persist across host restarts; the consumer checks
local handle recovery across restart before exercising strong transitions.

Run `sdks/kotlin/gradlew -p sdks/kotlin :qualification:inventory:check`. It compiles JVM target 17
with warnings as errors against coroutines 1.10.2 and kotlinx.serialization 1.9.0,
builds the Rust fixture and runs a real Kotlin consumer against the bundled SQLite
engine in a fresh directory under `build/`. This check is separate from `./scripts/check`.

## Budgets

JSON input/output contract frames are capped at 1 MiB. Queries permit 8,192 SQL
bytes, 256 parameters and pages of 1–1,024 rows and at most 1 MiB. A session permits 64 subscriptions, sharing its replica's observer
budget. The example host permits 128 active calls and 32 queued input/output
messages. Tokens are session-local; durable operation/request handles survive it.
The JVM fixture caps pending calls at 128 and uses one conflated Flow buffer.

These are development-profile budgets, not device memory guarantees.
