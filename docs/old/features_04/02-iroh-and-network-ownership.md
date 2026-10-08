# 2. Optional iroh plumbing and network ownership

Status: implemented 2026-10-06 and qualified on loopback only; see
[decision 0022](../decisions/0022-transport-independent-identity-and-peers.md),
[iroh networking](../runtime/iroh-network.md) and [SDK transports](../runtime/sdk-transports.md)
for the result and its limits. Owner: `rocky-iroh`, network service integration and SDK
starter composition. Depends on phase 1. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [SDK transports](../runtime/sdk-transports.md), [two-replica sync](../runtime/two-replica-sync.md),
[permissions/sync](../runtime/permissions-and-sync.md) and [replica roles](../runtime/replica-roles.md).
Inspect current iroh/TCP connectors, `NativeSession`, serving methods and SDK sync.
Use the pinned dependency's API/source for endpoint/router ownership behavior.

Deliver optional prebuilt Rocky handlers that attach to an application-owned
iroh endpoint/router, plus a quick-start helper that owns its endpoint. Keep
Rocky's authenticated sync, enrollment and command artifacts versioned.

## Ordered slices

1. Extract iroh dependency/configuration into `rocky-iroh`; database/command APIs
   and basic language bindings build without it. Keep TCP or in-process adapters
   as independent supported integration paths rather than SDK-owned defaults.
   Define a language-neutral stream/message adapter interface so a consumer can
   supply a transport without implementing a Rust trait or custom native build.
   Expose endpoint/protocol attachment through supported host-language handles;
   do not assume separately created native/Kotlin endpoints share one router.
2. Register handlers under explicit versioned protocol names without taking over
   every inbound connection. Application protocols can coexist on the endpoint.
   Closing a Rocky service releases its tasks/streams but not the borrowed router
   or endpoint; document the caller's final shutdown responsibility.
3. Separate stable domain/node identities from routes and transport kind. Define
   versioned enrollment/contact records with transport-independent identity and
   independently validated reachability hints. Migrate existing records or refuse
   incompatible opens; never trust an address as a data/command grant.
4. Allow sync with configured authorized peers, including non-creator peers.
   Preserve frontier paging, scope filtering, receipt/custody distinctions and
   checkpoint catch-up. The creator can remain an enrollment authority without
   serving every connection. Automatic elections/discovery are not implied.
5. Bound queues, concurrent connections, framing, timeouts, cancellation and
   reconnect work. Expose network status independently from database readiness.
   Document public relay/discovery dependencies and routing metadata disclosure.

An app may supply a kRPC-carrying protocol; no kRPC dependency or adapter is
required here. Demonstrate coexistence with a small app-owned echo protocol.
Do not make application RPC traffic pass through the coordinator or operation log.

## Completion gate

- One app-owned endpoint serves Rocky plus an independent protocol. Closing
  Rocky leaves that protocol usable; starter-owned endpoint shutdown is complete.
  The host-language attachment contract supports a Kotlin-defined app protocol
  without custom Rust code; an application-supplied transport has the same property.
- Two enrolled non-creator peers sync directly while the creator is offline,
  provided their credentials/keys are still valid. Denied peers remain denied.
- Domain state survives route/transport changes under the migration contract in
  [resolved choices](resolved-choices.md). Transport identity does not replace device/application authority.
- Disconnect/reorder/duplicate/backpressure tests preserve durable outgoing work,
  no partial bundle application and bounded resources.
- Build/test database and command consumers without iroh; test the supported
  iroh configuration separately. Record Internet qualification independently
  from loopback rather than claiming NAT traversal from unit tests.

Run affected Rust tests and `./scripts/check`; native/Kotlin checks if their
options change. Update transports, enrollment, module and lifecycle guides.
