# Public coordination service

A coordination service orders the strong commands of many applications'
domains. Its operator, the provider, is independent of every application
publisher; a self-hosted service is the same software with your own root key.
It is the [certified voter service](shared-coordination.md) with an optional
`PublicService` policy in its genesis: with it, tenants register their own
domains; without it, a mesh controls only its root-bound scopes. Operators read
the [operations runbook](coordinator-operations.md); evidence is in
[coordinator evidence](coordinator-evidence.md#public-service).

## What you trust, and what the service holds

| Question | Answer |
| --- | --- |
| Who is the service? | The voters a provider's root key certified. Your trust bundle pins that root, the mesh and the certified genesis; clients dial only its certified iroh endpoints and accept only voter-signed answers. |
| What does it learn? | Your domain's scope, its control certificate (owner, executor and invoker keys, generations, validity), request identities and commitments, timing and sizes. |
| What never reaches it? | Schemas, handlers, command arguments and results, rows, reader keys and credentials of your users. Results stay on your devices, verifiable with the voter-signed receipt evidence. |
| What can a voter do wrong? | Voters are trusted not to be Byzantine. A minority cannot forge an answer; a quorum outage leaves strong work pending, never confirmed by default. |
| Does it grant anything in my app? | No. Registering a domain or holding executor eligibility gives no row, decryption or enrollment right. Those stay in your publisher's credential chain. |

Mergeable writes commit locally whatever the service's state. A strong command
needs a quorum: during an outage it stays queued or unknown, and the same
identity completes once the quorum returns. `Unavailable` means unknown, never
failed. The [retention windows](coordinator-retention.md) apply: confirmation
within 24 hours of issue, responses retrievable for seven days unless
acknowledged; after that only the evidence your devices keep proves an outcome.

## Register a domain

A tenant is a `CoordinatorControl` key; it coordinates metadata only, so use one
per account, not per device. To register, it presents the publisher's
`SignedCoordinationGrant` binding publisher, application name and tenant key.
The coordinator derives the application ID from publisher and name, so nobody
can claim another publisher's application; domain and mesh IDs within it are the
creator's choice. A provider may also require a `SignedTenantAdmission` from its
admission key. Both tokens live in `rocky-credentials` (`rocky.coordination-grant.v1`,
`rocky.tenant-admission.v1`) and carry no row, key, command or enrollment right.

An issuer device, or any device whose credentials hold `Execute`, registers
through the database it already has open:

```kotlin
val owner = session.service(tenantKey)           // 32-byte tenant secret
owner.register(registration, executors = listOf(executor), invokers = listOf(executor, member))
owner.control(revision = 2, id, executors = listOf(member), invokers = listOf(executor, member))
owner.activate(id)                                // after a declared upgrade
owner.close(id)                                   // permanent; returns the closed head
owner.usage()
```

The same JSON calls serve every language binding:

| Request `type` | Inputs | Result |
| --- | --- | --- |
| `command_service_register` | `tenant` (64 hex digits, the secret), `registration` (32 bytes), `executors`, `invokers` (`command_identity` objects), `lifetime` (60 s to 366 days), optional `admission`, `grant` | `tenant`, `revision` 1, `control` |
| `command_coordination_grant` | `tenant` (public key) or `tenant_secret`, `lifetime` (1 s to 366 days); issuer only | `grant` |
| `command_service_control` | `tenant`, `id`, `revision` above 1, `executors`, `invokers`, `lifetime` | the same |
| `command_service_activate` | `tenant`, `id` | `active`, the activated `sequence` |
| `command_service_close` | `tenant`, `id` (32 bytes); owner only | the closed `StrongHead` |
| `command_service_usage` | `tenant` | open/closed domains, accounted bytes, registrations today |

An issuer signs the grant in the same call. Any other device needs `Execute` in its
[credentials](credential-primitives.md#raw-grants) and passes a `grant` an issuer signed
once with `command_coordination_grant` (Kotlin: `coordinationGrant`); a grant
for another publisher, application or tenant is refused before anything is sent.
The device then signs a `DomainControl` with the tenant key; only that control
certificate (at most 32 executor and 64 invoker keys) leaves the device. The
tenant secret stays in the process; its client keeps `tenant.evidence` and
`tenant.requests` in the database directory. Rust callers can use
`NetworkCoordinator::register`, `control` and `close_domain` directly.

The first executing device acquires the domain when it runs a command. After a
control revision the domain is unassigned until an eligible executor runs;
other devices meanwhile get the retryable `executor_unavailable`. A retry with
the same registration or control ID returns the same answer: the device keeps
each ID's signing time in `tenant.requests` and re-signs the identical control.
The same ID with other devices, lifetime or grant is `RequestConflict`.

The tenant owner can read its domain's head for closure and activation without
being listed as an executor or invoker. Other tenants' head reads remain denied.

A registered scope's authority is fixed: control revisions must be signed by
the registering tenant; registering a scope another tenant holds, or a closed
one, is refused. `ServiceOwner.close(id)` reads the current head and closes
there; retry the same ID after a lost reply. Each exact-head attempt has a
stable wire identity derived from that ID and the head, so a retry reading the
closed head still returns the closed head. Closing frees only the tenant's
open-domain quota: its marker still counts toward accounted bytes and the
service's 4,096 domains. A device without the tenant owner key cannot close it.
Moving a domain to another tenant or provider is a migration
and is not supported.

## Configure a device

A device names the service by its trust bundle, which `rocky-voter certify`
writes as `service/bundle.json` and the provider publishes after the group answers:

```json
{"format": 1, "root": "<64 hex: offline root public key>",
 "mesh": [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
 "genesis": "<lowercase hex CBOR of the certified genesis>"}
```

`mesh` is the 16 mesh-ID bytes as numbers; other bundle formats are refused.
In Rust, `Commands { coordinator: Coordination::Service(Box::new(TrustBundle::from_json(&bundle)?)), .. }`;
the starter's JSON options take the same object as `"coordinator"`, and Kotlin
composes `CommandRuntime(ServiceCoordinator(bundle), handlers)` (see the
[starter guide](getting-started.md)). The database signs requests with its node
key, keeps evidence in `coordinator.evidence` and reaches the service over its
own iroh network, so `Coordination::Service` requires iroh. The lower-level
client is `NetworkCoordinator::connect(&bundle, key, evidence_path, transport)`;
the transport stays its owner's. Changing service is a migration, not a
configuration edit: the evidence file is bound to one genesis and refuses another.

## Isolation, bounds and answers

Every request is authorized against its own scope's control: a tenant's keys
cannot acquire, control, look up, invoke, retire or close another tenant's domain.
Per tenant the genesis bounds open domains, accounted bytes (including closed
markers and sessions), daily registrations and retry outcomes; it refuses budgets
the service could not honour together (`tenants * tenant_bytes` within the state
budget, `tenants * retry_records` within the retry table). Lanes and retry
budgets belong to the request scope's tenant: a key eligible in two tenants does
not combine budgets; unauthorized requests share the unregistered lane.
Refused registrations hold no retry record. At most 4,096 domains, open and closed.

Each voter serves at most 64 concurrent client requests; each tenant holds at most
`tenant_lanes` (at most 32). A full lane answers `Backpressure` before consensus
work. The iroh handler keeps at most four inbound streams per connection in flight.
Defaults: 16 open domains and 4 MiB per tenant, six tenants per voter group.

| Answer | Meaning |
| --- | --- |
| `Denied` | Not your domain or application, a wrong grant or admission, or a key the control does not name |
| `Backpressure` | A quota or your request lanes are full; retry the same identity later |
| `Unavailable` | No authoritative answer; retry the same identity |
| `Configuration` | A control revision out of order, or a bundle or evidence file of another service |
| `RequestConflict` | A request ID already used for a different request; choose a new ID |

Tenants reach voters only over ALPN `rocky/coordinator/1` on certified iroh
endpoints, with the same signed frames as voter TCP; on a public service voter
TCP accepts client requests only from the membership authority. Head, lookup
and usage reads may report admission open for up to 30 seconds after it closed.

## Design rationale

- The provider root certifies voters, client endpoints, admission and budgets;
  it never signs an application's domain. Only the publisher's grant does, and
  the application ID is derived from publisher and name, never chosen.
- Quotas belong to the admitted tenant key, whatever login or billing preceded
  admission; the coordinator knows no accounts.
- Lanes refuse before consensus work and streams are capped per connection, so
  one tenant or connection cannot fill the queue that precedes authentication.
- iroh adds confidentiality only; voter signatures and the certified quorum
  still authorize every answer.
- A scope's authority is fixed at registration so that a control revision can
  never become a silent ownership transfer.

## Not provided

No cross-provider or cross-tenant migration, login provider, Byzantine fault
tolerance or placement across voter groups; a provider's capacity is one voter
group's. Unauthenticated connection floods are the provider's network concern;
Rocky bounds only what it accepts.
