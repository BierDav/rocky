# Typed strong commands

Typed commands of the secure profile: `rocky-core` descriptors, `rocky-secure`
execution, `rocky-sdk` tickets, the native ABI and Kotlin/JVM. Confirmation,
encryption and scoped results are those of
[encrypted commands](encrypted-commands.md); read that for the trust model.
The trusted profile keeps its [own command API](strong-publication.md) and
shares versions and the typed codec only.

A strong command is a transition one coordinator orders: of two reservations
competing for the last item, one is accepted and the other rejected, on every
device. Use mergeable writes for everything that may merge.

## Declare commands

Commands are part of the schema and of the domain's signed definition. Every
device declares the same ones; a device with another set is refused at open.

```json
{"application": "reservations", "version": 1,
 "collections": [
  {"name": "stock", "fields": [{"name": "remaining", "kind": "integer"}]},
  {"name": "holds", "fields": [{"name": "item", "kind": "text"}, {"name": "quantity", "kind": "integer"}]}],
 "commands": {"format": 1, "handler": 1, "commands": [
  {"name": "Reserve", "version": 1, "handler": "stock.reserve.v1",
   "arguments": [{"name": "item", "type": "text"}, {"name": "quantity", "type": "integer"}, {"name": "hold", "type": "text"}],
   "accepted": [{"name": "remaining", "type": "integer"}],
   "rejected": [{"name": "remaining", "type": "integer"}, {"name": "requested", "type": "integer"}],
   "invocation": "holds", "result": "holds", "resources": ["stock", "holds"]}]}}
```

| Field | Meaning |
| --- | --- |
| `version` | This entry's version. List one entry per version you still accept |
| `handler` | The declared identity of the code that decides this version |
| `arguments`, `accepted`, `rejected` | Ordered fields: `integer`, `real`, `text`, `blob` or `boolean`, optionally `nullable` |
| `invocation` | The table whose readers may see the arguments |
| `result` | The table whose readers may see the result |
| `resources` | Every table the handler reads or writes. All commands of a schema declare the same list |
| `commands.handler` | The handler generation. Raise it when handler semantics change |

A version that is not listed is refused, on the caller and on the executor.
Nothing is translated between versions: a handler receives the version the
caller sent. A command never reads mergeable rows. Declare its tables
[command-controlled](confirmed-tables.md) and SQL, drivers and Room DAOs read
the confirmed rows there, read-only. A resource left mergeable keeps its
confirmed rows separate from the mergeable table of the same name, which SQL
shows instead.

## Write a handler

A handler gets typed arguments and immutable keyed reads of its confirmed
tables, and returns one decision: accept with row writes and job intents, or
reject with a typed reason. A row is its `id`, then its columns in name order.

```kotlin
val reserve = Reserve handledBy { context, arguments ->
    val left = context.get("stock", arguments.item)?.get(1) as Long? ?: 0
    if (arguments.quantity > left) Decision.reject(Shortfall(left, arguments.quantity))
    else Decision.accept(Remaining(left - arguments.quantity), listOf(
        Write.Put("stock", listOf(arguments.item, left - arguments.quantity)),
        Write.Put("holds", listOf(arguments.hold, arguments.item, arguments.quantity))))
}
```

`Reserve` is a `Command<ReserveArguments, Remaining, Shortfall>`; the complete
example is the market's [`MarketCommands.kt`](../../examples/market-app/marketShared/src/MarketCommands.kt). Passing
another command's arguments, or reading its result as another type, does not
compile. In Rust, implement `rocky_sdk::CommandHandler` or, for compile-time
types, `rocky_core::TypedCommand`.

A handler must decide from its input alone. It has no database connection, no
key and no coordinator credential. Do not read a clock, a random number, a
file or the network: executors must reach the same decision, and a retried
call must too. The runtime cannot prevent this; it is the application's
obligation, and the handler identity is declared, not proven.

| Handler does | Result |
| --- | --- |
| Returns accept or reject | Prepared, confirmed and applied. A rejection is a confirmed result with no row change |
| Throws, panics or returns an error | That call fails as `handler_unavailable`; the ticket stays queued and runs again |
| Passes the deadline, 10 s by default | The same. The late answer is discarded |
| Returns a write outside `resources`, a wrong type, or writes with a rejection | Definite refusal as `rejected`; nothing is prepared |

One call runs at a time per database, on a thread the runtime owns. Eight
more may wait; further ones fail as busy. A stuck callback holds that thread
until it returns. Reads, writes and sync continue meanwhile.

## Keyed handler reads

Kotlin handlers use `context.get("stock", item)` or
`context.scan("stock", lower = listOf(item), limit = 20)`. Rust handlers use
`HandlerInput.reads`; native callbacks receive a
`NativeHandlerReads` handle alongside metadata-only JSON (handler frame version
2). `get(table, key)` returns exactly one row, or no row for an absent key.
`scan(table, lower, upper, limit)` returns at most `limit` rows, in ascending
primary-key order, with an inclusive lower and exclusive upper bound. Either
bound may be absent. Keys contain every primary-key component in declared key
order, including for compound keys; returned rows keep declared column order.

Limits must be 1 through 1,024. Unknown resources, null or incomplete keys,
reversed bounds and invalid limits fail explicitly. Equal bounds return no rows.
Components compare lexicographically: integers numerically, canonical reals
numerically, text by UTF-8 bytes and blobs by bytes. Distinct scalar types sort
integer, real, text, blob; they are not coerced. A differently typed key does
not match a row. This is a deterministic read contract, not SQLite collation.

Reads are immutable at the invocation's confirmed predecessor, including after
another command changes local state. The executor still verifies complete state
and rechecks that input before preparing; this API grants no new authority.
Native handles expire when their callback returns, including on error or unwind.
Only requested rows cross the ABI as tagged scalar JSON; invocation metadata
contains no table rows. Generated bindings and their native library must be
updated together.

The runtime still holds complete command tables and encrypted results still
carry complete before/after projections. This interface does not raise the
128 KiB envelope limit or qualify large command tables.

## Compose and provision

```rust
let open = Open { directory, schema, secrets: None, network,
    commands: Some(Commands { coordinator, handler: Some(handler), deadline: None }) };
```

```kotlin
val database = RockyDatabase.create(directory, schema, commands = CommandRuntime(coordinator, listOf(reserve)))
```

The coordinator is the application's and is never closed by Rocky. A device
without a handler submits commands and reads results. Without `commands`, a
database submits tickets and cannot run or settle them.

A coordinator must know the domain before the first command: who may execute
and who may invoke. `command_identity` returns a device's node and principal
for that. The native ABI ships `DevelopmentCoordinator`, an in-memory
coordinator shared by the databases of one process. Its ledger ends with the
process, after which the domain makes no strong progress. A coordination
service is named by its trust bundle: `Coordination::Service` in Rust or the
starter's `coordinator` option, with a domain its owner registered; see
[public coordination](public-coordination.md). An executing device of a service
acquires an unassigned domain itself.

Execution needs three things, and reaching a device is none of them: the
coordinator assigns it as executor, its credentials hold `Execute` on every
resource, and it holds the complete confirmed state. Among the starter profiles
only the issuer gets `Execute` and writers of the invocation table get `Invoke`;
a [raw grant](credential-primitives.md#raw-grants) gives either to any device, which then
registers as an executor itself.

## Tickets

`submit` stores the signed invocation and returns a ticket. It contacts
nobody. Persist the handle first and retry with it: the same handle and
arguments return the same ticket, other arguments fail as `conflict`.

| State | Meaning |
| --- | --- |
| `queued` | Stored here. Nothing was sent or prepared |
| `prepared` | This device prepared the transition and has not submitted it |
| `outcome_unknown` | It left this device, or a confirmation was submitted, without an established answer |
| `confirmed` | The coordinator's receipt is verified. Rows this device reads are not all applied, often because earlier results arrive with the next sync |
| `applied` | Confirmed, and applied to every table this device reads |
| `cancelled` | Withdrawn before it left this device. It never runs |
| `expired` | Admission closed without confirmation; never runs under this identity |
| `retired` | Recovery horizon or acknowledgment floor passed; outcome unavailable |

`outcome` is the typed result once confirmed: `accepted` or `rejected` with
values, or `unavailable` when this device cannot read the result's table.
`custody` lists peers that acknowledged holding or applying the result.
A failed run reports an `error` with a code beside the ticket. A rejection is
an outcome, never an error.

Run a ticket in `outcome_unknown` again; never submit a replacement. The
request identity is stable, so a confirmation that was already accepted is
found and its retained result applied, without a second decision. After the
[recovery horizon](coordinator-retention.md), it becomes `retired` or `expired`;
no new identity is allocated automatically. Cancelling
withdraws only a ticket that never left the device; otherwise it returns the
ticket unchanged. At most 64 tickets may be unresolved per device; a further
`submit` fails as `quota`.

## Run and deliver

`command_run` advances one ticket or all unresolved ones. The coordinator
names the executor. If it is this device, the command executes here.
Otherwise the SDK looks up its configured or learned contact. With none, it
asks admitted contacts for the node ID the coordinator names and verifies that
the returned key hashes to that ID before retaining it. It tries a direct
route first. If direct delivery fails, an admitted contact forwards the
unchanged signed envelope directly to the executor and returns its answer.
Forwarding is one hop; a peer without a route refuses so another can be tried.
The ticket stays unresolved with a retryable `executor_unavailable` error
when none reaches it. A caller need not configure or previously meet the executor.

A forwarder permits eight concurrent commands per caller and refuses a ninth
as retryable. It holds no queue or durable work and releases its slot after
an answer, cancellation or a 30-second timeout. Carrier signatures authenticate
both peers; the executor checks the original caller's chain and the held
revocation set, and the caller verifies the result and coordinator receipt.
A forwarder sees identities, grants, methods, commitments, sizes and timing;
arguments and results stay encrypted unless it already holds their reader keys.
See [decision 0059](../decisions/0059-command-forwarding.md). Both paths pass
the same admission: signature, current credentials, registered method, supported version and invocation
scope. A device that receives a command it is not assigned to execute refuses.

To forward commands through your own RPC or gateway instead, use
`command_envelope` on the caller, `command_execute` on the executor and
`command_settle` on the caller. The envelope is signed by the device that
submitted it, so whoever carries it gains nothing, and a changed byte is
refused. Results reach other devices with `sync`, each filtered to the tables
the receiver may hold. Rows sync whether or not results could be fetched; a
ticket then stays `confirmed` until a later sync.

## Calls

| Request `type` | Inputs | Result |
| --- | --- | --- |
| `command_identity` | | `node`, `principal`, `scope`, generations, whether a handler and a coordinator are composed |
| `command_submit` | `handle` (32 bytes), `method`, `version`, `arguments` (tagged values) | Ticket |
| `command_ticket` | `handle` | Ticket, or `{"state": "unresolved"}` |
| `command_tickets` | | Unresolved tickets in submission order |
| `command_cancel` | `handle` | Ticket |
| `command_run` | optional `handle` | `tickets`, each with the `error` of this run or null |
| `command_envelope` | `handle` | `envelope` bytes; the ticket becomes `outcome_unknown` |
| `command_execute` | `envelope` | `answer` bytes: confirmed, not submitted, unknown or settled |
| `command_settle` | `handle`, `answer` | Ticket |
| `command_rows` | `table` | `complete`, the confirmed `sequence` this device holds results through, `columns`, `rows` |

Values are tagged as in [the call contract](sdk-contract.md#ownership-and-wire-encoding).
Without `complete`, `command_rows` withholds the rows; it never returns an
invented empty table. Error codes added here: `handler_unavailable`,
`unknown_confirmation`, `missing_head`, `executor_unavailable` (all retryable
with the same ticket), `stale_fence` and `quota`. The trusted runtime settles
`StaleHead` or `StaleEpoch` by atomically discarding the refused preparation and
returning its request to queued, then flushing before returning `stale_fence`.
A queued request can be released; otherwise the next pass prepares the same
identity and sealed cut against the new head. A certified confirmation with a
lost reply still yields its receipt even after another executor advances the
head. An executor behind the confirmed predecessor
reports `missing_head` until a sync brings the results. A domain with no assigned
executor reports `executor_unavailable`, and an executor that does not answer
reports `unreachable`. `complete` with a `sequence` lower than another device's is
consistent but older state.

## Formats

`rocky_core::CommandDefinitions` is the JSON above; `build` returns a
`CommandSet` of canonical `CommandDescriptor`s.

| Bytes | Layout |
| --- | --- |
| Descriptor | `rocky.command.v1`, name, `u32` version, handler, three field lists, invocation, result, sorted resources. Text is a `u32` length and UTF-8; a field is name, kind byte and nullable byte |
| Descriptor id | `commitment("rocky.command.descriptor.v1", descriptor)` |
| Arguments | format byte 1, kind byte 1, descriptor id, `u32` count, values |
| Outcome | format byte 1, kind byte 2 (accepted) or 3 (rejected), descriptor id, `u32` count, values |
| Manifest method shape | `rocky.command.v1:` and the hex commitment over every version's `u32` version and id, ascending |

Values use the `SqlValue` codec; a boolean is the integer 0 or 1. Decoding
re-encodes and compares, so another type, a missing or extra value and
trailing bytes are refused. The manifest holds one method per command name and
the set's `handler` generation; a changed command set is another manifest.
Byte-oriented methods without a descriptor are version 1
(`RowCommandApplication::supports_version`).

`secure_tickets` maps each submitted handle to its request hash.
`secure_command_progress` holds per request: 1 prepared, 2 outcome unknown,
3 cancelled, 4 confirmed with its sequence. State 2 is written and flushed
before a confirmation is submitted or an invocation leaves the device; an
explicit coordinator refusal returns it to 1, and an executor's answer that it
submitted nothing clears it. State 4 commits with the applied result.
`Protocol::Command` (`rocky/command/2`) carries a signed request and a signed,
request-bound reply around the unchanged `EncryptedCommand` and `Delivery`.
The earlier command protocol is refused. [Decision 0059](../decisions/0059-command-forwarding.md)
defines the direct and forwarding carrier.

## Limits

- The secure profile needs a coordinator reachable by every device that
  executes or settles. `DevelopmentCoordinator` is not durable.
- A handler reads selected keys and bounded ranges; a result still carries
  each table's rows before and after. Command envelopes stay under 128 KiB,
  so command-controlled tables are small tables.
- 32 row writes and 31 jobs per decision; arguments and results up to 48 KiB.
- Job intents are retained with their stable identity; dispatch stays with
  [the effect API](recovery-and-effects.md#durable-effects) of the secure runtime.
  The SDK calls do not list or dispatch jobs yet.
- Tickets, preparations and results share the command budget of 4,096 records
  and 32 MiB and are not pruned. An executor filled it after 938 commands in
  the [end-to-end run](end-to-end-evidence.md#device-limits).
- Executors are issuer devices unless a raw grant says otherwise; no scoped
  executor profile exists. 32 executions may wait in the runtime.
- No arbitrary strong SQL or transaction closure is offered: a command is an
  explicit, declared transition.

## Qualification

```sh
cargo test -p rocky-core --locked
cargo test -p rocky-secure typed_commands
cargo test -p rocky-sdk --test commands
cargo test -p rocky-runtime typed_reservation
./sdks/native/check                 # read-handle lifetime and supported features
sdks/kotlin/gradlew -p sdks/kotlin :rocky-core:jvmTest         # keyed-read codecs
sdks/kotlin/gradlew -p sdks/kotlin :qualification:inventory:check  # compile-fail fixtures
./examples/market-app/check         # Kotlin keyed reads through the native runtime
./examples/market-app/qualify       # JVM processes against three voters
```

Covered: competing callers, forged inputs and grants, lost confirmation
answers across reopen and takeover, handler failure and deadline, and that no
descriptor, argument or row reaches the coordinator; in one process or over
loopback, against the in-memory coordinator. Contention, loss, takeover and
voter faults as separate processes against three voters are in
[market qualification](market-qualification.md).

## Design rationale

- Handlers run outside the replica lock and write transaction, on a thread the
  host owns. `spawn_blocking` around a synchronous trait method was rejected:
  the deadline and queue belong to the host, not a shared blocking pool. The
  input is re-read inside the preparation transaction instead.
- A rejection is a confirmed outcome, not an error: only so is it deduplicated
  and ordered against competing commands.
- One scoped result sits beside the scoped row projections, which already
  filter per reader; a typed result section per reader scope was rejected.
- Every argument and outcome carries its descriptor id, binding method,
  version, handler identity and field types. Versions are never translated.
- All commands of a set declare the same resources: one confirmed sequence
  orders them and an executor proves the predecessor of every scope it reads.
- An invocation is signed by its originating device with its chain, so a
  forwarding transport or gateway gains no authority; local tickets and
  delivered commands pass the same `admit_command`.
- Command-controlled table shapes cannot transition; a changed class or shape
  needs a new domain ([confirmed tables](confirmed-tables.md)).
