# 5. Typed strong commands with independent delivery

Status: implemented 2026-10-06; see [typed commands](../runtime/typed-commands.md),
[decision 0025](../decisions/0025-typed-commands-tickets-and-handler-hosts.md) and the
[evidence and limits](#evidence-and-limits) below. Owner: application contracts, trusted/secure command runtimes
and SDK bindings. Depends on phases 1–4. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [encrypted commands](../runtime/encrypted-commands.md), [strong publication](../runtime/strong-publication.md),
[strong contract](../protocol/strong-command-contract.md), [effects](../runtime/recovery-and-effects.md)
and [old phase 7](../old/features_03/07-strong-state-and-effects.md).
Inspect `CommandHandler`, `RowCommandApplication`, preparation and result admission.

Deliver explicit typed command definitions and durable progress tickets. This is
the largest novel engineering item in the roadmap; plan its schedule accordingly. Application
RPC is independent; network calls cannot manufacture confirmation or bypass
admission. A command targets an invariant domain, with execution eligibility
separate from merely discovering/reaching a peer.

## Ordered slices

1. Define stable command identity/version, argument and business-result/error
   descriptors/codecs in the installed application definition. Deterministic,
   versioned encoding must bind method/input/result types and avoid ambiguous
   defaults or unordered serialization. Keep schema/handler/policy generations
   distinct. Retain old command bytes under an explicit compatibility policy.
2. Add typed registration/invocation/results with language-neutral descriptors
   and a non-Rust definition/registration path. Support existing Rust handlers and
   an application-language handler host, with Kotlin/JVM as the first consumer,
   implemented through foreign callback interfaces per
   [decision 0020](../decisions/0020-in-process-uniffi-bindings.md).
   Callbacks receive immutable input/state bound to the predecessor and return
   bounded deterministic changes/scoped outputs. Run host code outside database
   transactions and native locks; revalidate head/authority before preparation or
   confirmation. Define stable handler identity, deadlines, callback failure,
   scheduling, codecs and reopen/retry semantics. No arbitrary SQL connection,
   side effects or coordinator credentials are delegated to callback code. Malformed
   remote bytes still fail runtime validation. Clients need definitions, not
   handler execution rights. Handler identity is declared and installed; trusted
   executors are relied on to run it, as V4 states. It is not a cryptographic
   proof of the executed code.
3. Separate local durable submission and status/outcome lookup from transport
   delivery. Persist stable identity and exact input before returning a ticket.
   Retry the same ticket; changed input under it conflicts. Bound queues/history
   and expose backpressure. General peer lookup is not part of the command API.
4. Use existing executor authorization, exact inputs, predecessor/fencing checks,
   immutable preparations, confirmation and atomic scoped result application.
   Maintain trusted sealed-cut versus encrypted confirmed-state semantics.
   A mergeable overlay is not an automatically confirmed command input.
5. Tickets distinguish queued, prepared, outcome unknown, confirmed, locally
   applied and independently acknowledged custody. Typed business rejection is
   separate from transport/storage/authority failures. Cancellation stops waiting
   or sending where possible; it cannot roll back a confirmed transition.
6. Preserve durable job/effect intents and provider keys; no network calls in
   deterministic handlers, transactions, replay or migration. An explicit command
   API does not claim arbitrary strong SQL/Kotlin transaction support.

The coordinator receives commitments/generations, not command descriptors or
schemas. Keep application RPC optional; a thin gateway may call this API under an
explicit forwarding-signature identity model ([resolved choices](resolved-choices.md)). No kRPC dependency
is required. Typed results must retain reader scopes rather than flatten private outputs.

## Completion gate

- A Kotlin-owned reservation handler registers/executes through the packaged
  runtime without Rust source or a custom native build. Callback timeout/crash,
  concurrent head change and single-thread scheduling preserve safe recovery.
- A reservation example exposes typed input and accepted/rejected result, with
  a compile-fail fixture proving mismatched argument/result use is rejected.
- Two callers competing for one item cannot both confirm acceptance in one
  invariant domain. Business rejection and protocol failure are distinguishable.
- Local and adapter-delivered submissions use the same admission path; forged
  method/version/payload/grants fail. Non-eligible peers cannot execute.
- Lost confirmation reply, cancellation, takeover and reopen recover the same
  ticket/result without a second transition or a new identity.
- Public/private result subsets verify without exposing unavailable scopes;
  missing payload/predecessor remains unavailable. Receipts grant no row access.
- Existing effects and both command profiles retain their fault tests. No
  application payload/descriptor appears in coordinator state.

Run command/secure/coordinator/effect suites and `./scripts/check`; run binding
checks if changed. Document actual APIs and leave retention changes to phase 6.

## Evidence and limits

Recorded 2026-10-06 on macOS arm64. `./scripts/check`, `./sdks/native/check`,
`./sdks/kotlin/check` and `./examples/shoppinglist-app/check` passed.

| Gate | Evidence |
| --- | --- |
| Kotlin-owned handler through the packaged runtime; timeout, crash, head change, single-thread scheduling | `ReservationCommandTest` in `examples/shoppinglist-app`; `commands::tests` in `rocky-sdk`; `handler_failures_and_a_moved_head…` in `rocky-secure` |
| Typed input and accepted/rejected result with a compile-fail fixture | `Reservation.kt` and `sdks/kotlin/fixtures/compile-fail`; `TypedCommand` doc tests in `rocky-core`; `inventory-app` version 2 |
| One acceptance among competing callers; rejection apart from protocol failure | `competing_reservations…` in `rocky-secure`; `competing_callers…` in `rocky-sdk` |
| One admission path; forgeries and non-eligible peers fail | `local_and_delivered_commands_share_admission…` in `rocky-secure` |
| Lost reply, cancellation, takeover and reopen recover the same ticket | `lost_reply_reopen_cancellation_and_takeover…` in `rocky-secure`; `tickets_survive_reopen…` in `rocky-sdk` |
| Scoped result subsets; receipts grant no access | `scoped_results_stay_scoped…` in `rocky-secure` |
| Existing effect and command fault tests; nothing of the application in coordinator state | Unchanged suites of `rocky-secure`, `rocky-runtime` and `rocky-coordinator`; recorded proposals in `scoped_results_stay_scoped…` |

Limits of this phase:

- The packaged runtime composes only the in-memory `DevelopmentCoordinator`.
  The Kotlin evidence uses two databases in one process. Phase 7 adds shared
  service configuration to the JSON starter; executing Kotlin service handlers
  remain phase 8 composition work.
- The typed layer is the secure profile's. The trusted profile gained command
  versions and the typed codec for `inventory-app`, not tickets or a handler host.
- Executors are issuer devices. A stuck foreign callback cannot be interrupted;
  it holds the handler thread until it returns.
- A handler receives every row of its tables, so command-controlled tables
  must stay small. Job intents are retained but not listed by SDK calls.
- Handler determinism is trusted, as V4 states. Crash evidence is reopen of the
  same files; a JVM crash was not injected, and nothing ran outside loopback.
