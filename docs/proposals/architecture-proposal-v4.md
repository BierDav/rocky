# Rocky DB: composable P2P application infrastructure

Status: accepted product and architecture direction, 2026-10-06. Implementation follows the
[roadmap](../features_07/README.md). This proposal supersedes V3
for future work; implemented guides continue to describe the code as it exists.

## Product contract

Rocky supplies local-first database, replication, credentials, strong commands
and optional networking infrastructure that developers can compose into apps.
The integrated starter setup uses optional iroh plumbing and a public coordinator
operated by the Rocky provider. Users run application nodes on their devices;
developers do not need to deploy a backend to start.

The Supabase comparison describes a coherent set of application building blocks,
not PostgreSQL compatibility or a promise to implement every backend service.
Applications can later add always-on executors, replica/custody nodes or their own
coordinator. A public coordinator is shared infrastructure, not a serverless
application host, database backup or general peer directory.

## Language, ORM and transport independence

Every capability must have a documented language-neutral contract and an adapter
boundary that can theoretically be implemented for any programming language,
ORM or transport capable of meeting its semantics. Rust is an implementation
choice, not a consumer prerequisite. Getting started includes defining schemas,
credentials, commands and application handlers without a Rust project, Cargo,
Python, editing Rust or compiling a custom native library.

Ship prebuilt runtime/binding artifacts and selected language adapters. Additional
languages may use generated bindings or a packaged local runtime protocol where
native linking is unsuitable; specify the trust/lifecycle boundary rather than
exposing a raw database connection. Application-language handlers operate on
immutable typed command input/state and return bounded deterministic changes and
scoped outputs. The framework supplies execution, admission and persistence;
callbacks never own DB transactions. Deterministic encoding and handler-generation
identity apply across languages, not just within Rust.

Consumers run the runtime in-process through generated UniFFI bindings; a sidecar
process is not planned ([design rationale](../runtime/sdk-packaging.md#design-rationale)).
The first qualification target is Kotlin/JVM desktop, including application-owned
command handlers. Any-language extensibility is an architecture requirement,
not a claim that all language/platform adapters ship immediately. Runtime languages
and platforms must actually support the selected adapter's execution/security
contract. An ORM/transport adapter cannot promise unsupported distributed semantics.

## Capability boundaries

| Capability | Owns | Optional dependencies |
| --- | --- | --- |
| Shared contracts | Identities, schema descriptors, versioned protocol evidence and ports | No storage, transport, ORM or UI implementation |
| Database/runtime | Managed SQLite transactions, precise capture, projections, operation storage, outbox, replication admission and committed observations | Credential verification for secure sessions; explicit sync service |
| Credentials | Signed manifests/grants, delegation, policy verification and identity/key-provider contracts | Optional enrollment service and login-provider adapters |
| Strong commands | Durable invocation, deterministic preparation, confirmation recovery, scoped results and progress | Managed durable storage, authorization and a coordinator |
| Coordinator | Registration/control metadata, heads, generations, executor fencing, confirmation and bounded retry evidence | Provider-operated consensus/storage and authenticated client API |
| Iroh integration | Rocky protocol handlers, dialing and bounded stream handling | Application-owned endpoint/router or optional owned starter endpoint |
| SDK/bindings | Language APIs, handles, cancellation and the lifecycle of resources they own | Independently selected services; optional starter composition |
| Application | Schema, merge rules, command definitions/handlers, trust and business policies | Its own RPC/API protocols and effect workers |

Capability separation does not mean arbitrary combinations work. A local database
needs no network or coordinator. Secure replication requires credentials and keys.
Strong commands require managed durable state and coordination. Enforcement stays
inside the runtime at reads, admission and commit even when credential issuance
is supplied by a separate module. Unsupported combinations fail explicitly.

Keep `rocky-core`, `rocky-storage`, `rocky-runtime`, `rocky-secure` and
`rocky-coordinator` as existing foundations. Phase 1 defines the supported ports
and migration path. Introduce a reusable `rocky-credentials` module and
`rocky-iroh` integration at real replacement boundaries. A strong-command module
has its own API and optional integration; a separate crate is warranted only if
it preserves atomic storage ownership without a dependency cycle.

## Application definitions stay on application nodes

Developers package logical schemas, replication/security policies and typed
command definitions with their applications. Replicas record installed definition
identity and schema evidence locally and validate it on reopen. Executors install
the registered deterministic handlers in the application language through the
supported host interface; existing compiled Rust handlers remain supported.
Clients can carry command argument/result bindings without being eligible executors.

The coordinator stores fixed-size commitments/identifiers and accepted generation
numbers, never schema documents, field catalogs, argument/result schemas,
executable handlers, rows, encrypted application payloads or reader keys. It
checks generic authority, predecessor and generation metadata. App nodes check
schema compatibility and business rules. Schema distribution, if later needed,
is a separate application/peer service.

## Database and ORM contract

SQLite WAL remains the first supported engine. Shared-file atomic commits include
projection, operation, outbox, retry outcome and revision changes. Network and
coordinator calls occur outside transactions; publication follows the required
durable commit. Keep the trusted and encrypted profiles distinct.

Developers may use SQL or any qualified ORM adapter over the managed session.
An independent connection cannot write replicated tables while bypassing Rocky's
commit boundary. An application-owned database may instead be an explicit
projection; that is a different consistency contract. Room is one optional
consumer and schema importer, not the source of the generic schema contract.

Offline mergeable tables have declared conflict rules. Global uniqueness,
reservations, balances and ownership transitions require explicit strong semantics.
Local atomicity is not cross-file or distributed serializability. Missing keys,
coverage or required companions yield availability errors, not fabricated empty
rows. Query events report committed local views, not global freshness.

## Networking and application RPC

Opening a database does not start, own or require a transport. Runtime services
consume authenticated protocol inputs and emit durable outgoing artifacts;
network services deliver them with bounded framing, queues and retries.
The application chooses peers, scheduling and connection lifecycle, with optional
starter defaults. Domain identity is independent of transport kind and address.

`rocky-iroh` supports both attachment to an existing endpoint/router and a
quick-start endpoint it owns. Closing a database or Rocky handler must not close
an application-owned endpoint. Synchronization can use eligible peers directly;
the domain creator may bootstrap enrollment but is not a permanent routing hub.
No unrestricted public discovery or automatic mesh-wide replication is implied.

Applications may register their own protocols, including an adapter carrying
kRPC messages, or use REST/gRPC through a gateway. Rocky implements neither a
general RPC framework nor a mandatory client-to-application protocol. RPC can
call the Rocky command API, but its request IDs, cancellation and transport
success are not substitutes for durable command identity or confirmation.

## Strong commands and typed APIs

A strong command is an explicit installed deterministic transition ordered within
one invariant domain. The executor needs complete invariant-relevant state and
the required invocation/execution/effect rights. Authorized executors are trusted
to run the installed application correctly; the coordinator is not a correctness
proof for arbitrary business code. Voters use the existing crash-fault trust model.

Persist a stable invocation, validate/prepare immutable result bytes, confirm the
commitment against the fenced predecessor, then atomically apply authorized
results and publish after commit. Requests/results can travel over optional
adapters; application RPC remains independent. A receipt grants no read rights.

The existing trusted sealed-cut and encrypted confirmed-state paths remain
distinct. Mergeable data does not silently become confirmed command input.
Cross-domain commands are workflows unless a separate atomic design is accepted.
Handlers perform no external I/O; durable scoped jobs dispatch effects afterward
with explicit uncertainty and provider idempotency where available.

Define stable method identity, version, argument/result codecs and typed business
outcomes in the application definition. Provide typed host-language APIs and
generated bindings over versioned protocol bytes. Schemas and commands have a non-Rust
definition/registration path; clients and executors need no custom Rust build.
Keep transport failure separate from business rejection. Tickets distinguish
durable queued, prepared, outcome unknown, confirmed and locally applied states,
with named custody evidence separate.
Type safety does not replace authorization, runtime decoding or compatibility checks.
Arbitrary Kotlin/SQL transaction bodies are not remote deterministic handlers.

## Public coordinator and bounded storage

The provider hosts a shared durable coordinator as the default convenience.
Registration isolates domains and binds owner authority, executor eligibility,
generations and resource quotas without deploying app code. An optional Directory
application or external login provider may help enrollment; neither is required
to define application schemas or silently confer application rights.

Selecting a self-hosted coordinator is supported at initial setup. Moving an
existing domain to another coordinator requires a fenced, verifiable trust/state
migration; changing a URL is insufficient and is deferred from the first release.

At fixed live domains and admitted device/session counts, retained coordinator
storage must plateau under sustained traffic. A large row set adds no coordinator
payload state; additional domains and devices do. The provider retains current
control/head state, compact replay barriers and bounded response/recovery windows.
Consensus logs, snapshots, backups and operational logs also need bounded policy.

Full historical outcomes move to application-owned receipt/result retention or an
optional archive. Old identities are rejected after retirement, never admitted
as new commands. Lookup distinguishes retained outcome, safe not-confirmed evidence
and historical outcome unavailable. Missing records alone do not prove failure.
Expired sessions and retired domains cannot be recreated under old identities.

Count and byte caps, bounded outstanding work, safe session expiry and current
domain lifecycle bound traffic/device churn. An offline device cannot pin public
records indefinitely. Exact concurrency, clock, retirement and evidence rules
must pass retention qualification before launch. Existing retain-all snapshots and quota refusal do not meet this
contract. No indefinite lookup promise or automatic new-ID retry is made.

The public service publishes its recovery window and limits. Coordinator outages
pause strong progress while admitted mergeable edits continue. Payload loss is
independent: a commitment cannot restore lost data. Optional custody/backup nodes
provide availability; the coordinator is not the only-copy custodian.

## Scope and implementation authority

The [phase briefs](../features_07/README.md) are the current implementation path.
Unfinished Room prerequisites are split into generic work and deferred adapters;
transparent strong DAO writes, universal ORM compatibility, alternate database
engines, complete blob/presence services, mobile/web parity and feature parity
with other products are outside this release.

Existing performance targets and failed gates remain visible. Architectural
alignment does not count as qualification. Protocol/storage changes require
explicit migration or incompatible-open refusal, decision records and invariant
tests. Every phase updates current guides and reports evidence and limits.
