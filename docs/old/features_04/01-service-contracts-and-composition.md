# 1. Service contracts and composition

Status: implemented 2026-10-06; see [decision 0021](../decisions/0021-service-boundaries-and-resource-ownership.md)
and [SDK composition](../runtime/sdk-composition.md) for the result and its limits. Owner: `rocky-core`, runtime/secure integration and `rocky-sdk`.
Depends on existing schema/session/bundle foundations and
[decision 0020](../decisions/0020-in-process-uniffi-bindings.md). Follow the
[execution contract](README.md#agent-execution-contract) and [V4](../proposals/architecture-proposal-v4.md).

## Inputs and objective

Read [modules](../modules.md), [SDK contract](../runtime/sdk-contract.md),
[enrollment](../runtime/sdk-enrollment.md), [transports](../runtime/sdk-transports.md)
and [secure sessions](../runtime/secure-sessions.md). Inspect `Engine::open_with`,
`Node`, enrollment/sync callers, secure admission, coordinator ports and the native
ABI. Map responsibilities and dependencies, not every file.

Deliver independently constructible services and one optional composition owner.
Opening the database alone must require no endpoint, enrollment connection,
coordinator contact or public discovery. Credentials/keys may still be required
to open secure state; do not replace them with trusted defaults.

This phase is deliberately narrow: the dependency decision, ownership rules and
the open-versus-attach split. Detailed contracts belong to the phase that first
exercises them: transport adapters in phase 2, credential and key-provider
contracts in phase 3, schema descriptors in phase 4 and handler hosting in phase 5.

## Ordered slices

1. Record the dependency direction from [resolved choices](resolved-choices.md)
   and the resource-ownership table in a decision record. Define database, credentials, command and network ports;
   share inert protocol/identity types without creating cycles. Keep transaction
   execution in the runtime. A separate command crate must not become a second
   writer or require sequential projection/outbox commits. Cross-boundary data
   is versioned protocol bytes or descriptors per decision 0020.
2. Separate existing SDK construction into explicit local open and attachable
   services. The starter wrapper may retain `create/join/resume` convenience;
   these must compose lower APIs rather than be the only route to the database.
   Preserve the current SDK as a thin facade over the new services
   ([resolved choices](resolved-choices.md)); record old persisted options and transport-kind handling and do
   not silently reinterpret existing enrollment state. Export no raw SQLite writer.
3. Define ownership for supplied endpoints, keys, coordinator clients, background
   tasks and shared database sessions. Service close drains its work, is bounded
   and idempotent, and does not close borrowed infrastructure or another session.
   Local-only, secure replication and strong-command profiles state their
   prerequisites; invalid combinations fail before side effects with structured
   errors. Optional does not mean insecure.

Introduce only exercised ports with a real adapter and consumer. Phase 2 extracts
iroh, phase 3 credentials, and phase 5 the typed command layer; do not complete
those by adding placeholders in phase 1. Keep the existing trusted/secure paths
and runnable consumers usable while changing composition.

## Completion gate

- Open/read/write/reopen a secure local database with installed credentials and
  no networking or coordinator. A test-controlled network factory proves no dial.
- Compose the existing encrypted sync path through the service boundary and
  retain its atomicity/admission/retry tests.
- Two sessions share a database owner; closing one does not shut down the other.
  Borrowed infrastructure survives service close; owned resources are released.
- Existing native/Kotlin consumers work through the facade, or receive explicit
  incompatible-version errors with documented migration steps.
- The dependency map, public APIs, configuration profiles and ownership rules are
  documented. No new crate reports success for unimplemented behavior.

Run affected Rust package tests, `./scripts/check`, and native/Kotlin checks when
their boundaries change. Update [modules](../modules.md) and the SDK guides.
