# Phase 16: strong commands in the mesh

Status: complete locally on 2026-10-11; hosted CI not run.
[The brief](16-strong-commands-in-the-mesh.md)'s acceptance gates and shared
checks passed locally. No test-budget exception is added.

## Audit

The command envelope verifies its original node chain against the held
revocation set. It does not consult a configured-peer list, an enrolling
issuer or a linear policy epoch. `request.policy` names the signed application
manifest, not a revocation epoch. Result parts carry individual `KeyEpoch`
identities; retained sibling secrets remain usable. Executor grants are
checked again at preparation, recheck and confirmation. The coordinator owns
fence ordering; the SDK does not replace it.

The SDK's remote delivery did require the executor among contacts and refused
instead of requesting one or forwarding. Command protocol 2 replaces this
path with contact resolution and an authenticated one-hop carrier. Carrier
admission uses the secure runtime's generation horizon and revocation set.
The carrier adds no new ticket, result or receipt encoding.

Confirmed history is verified by its coordinator receipt, with historical
chains independently of later revocation. New execution uses current
credentials. Results continue to sync from custody. The audit covers market `MarketCommands.kt`/`Market.kt` and shopping
`Approvals.kt`/`Coordination.kt`. Shopping registers every live trusted device
as eligible; no enrolling issuer is privileged by command admission. Its app
roster remains Phase 20 work. Both use this generic command path; application handlers do not own admission,
key selection, transport or fences.

## Coverage and measurements

- SDK `mesh_buyers_find_an_executor_through_only_a_neighbor`: contact discovery,
  admission without an executor-side contact allow list and typed shortfall.
- SDK `mesh_forwarding_and_direct_replay_decide_one_ticket_once`: no direct
  route, one-hop delivery, direct replay, altered envelope refusal at the
  executor and altered answer refusal at the caller.
- SDK `mesh_revoked_caller_is_refused_without_another_handler_call`: definite
  refusal and unchanged confirmed head.
- SDK `mesh_forwarded_revoked_caller_is_refused_at_the_executor`: only the
  executor knows the caller's revocation; forwarding still produces a definite
  refusal without deciding.
- SDK `mesh_revoked_forwarder_is_refused_at_the_executor`: the executor
  refuses the forwarding peer's revoked node chain without deciding.
- SDK `mesh_lost_confirmation_reply_retries_through_the_forwarder_without_deciding_again`:
  retry across the same forwarding route recovers the receipt, not a new decision.
- SDK `mesh_without_a_route_keeps_the_same_unresolved_ticket`: retryable route
  exhaustion preserves the request identity and unresolved outcome.
- SDK `mesh_offline_caller_settles_from_custody_alone`: an offline caller's
  command confirms, custody collects the result, the executor closes, and
  only custody supplies the caller's applied outcome.
- SDK pure carrier-signature and slot tests: signed envelope, route and answer
  binding; ninth request refused, independent caller admitted, no idle slots.
- SDK `mesh_ninth_concurrent_forward_is_retryable_without_queuing`: eight
  caller deliveries stopped at the direct dial boundary, ninth refused without
  dialing, then every original ticket completes after releasing the gate.
- SDK `mesh_forwarder_restart_drops_inflight_work_and_same_ticket_completes`:
  deterministic in-flight dial interruption, service drain and reopen; no
  forwarding queue survives. This is e2e because of the production 5 s drain.
- Core protocol test: protocol 1 is unknown; protocol 2 is current.
- Secure `mesh_another_issuer_revokes_executor_and_successor_finishes_pending_ticket`:
  another authority's statement stops confirmation, the predecessor receipt
  stays verifiable and the successor completes the caller's original ticket.
- Secure `mesh_two_executors_fencing_at_once_preserve_the_head_and_pending_ticket`:
  simultaneous expected-epoch fencing leaves one winner and the same head,
  followed by one decision on the pending ticket.
- Existing contact-request substitution test rejects a wrong node hash.
- Market `mesh`: buyers enroll through custody, publish identities through it,
  keep only its contact and request the executor's route to reserve the last item.

The SDK integration cases measured 169–539 ms alone after the tier runner's
load normalization, below the unchanged 2 s budget. The two secure cases
passed together in 0.20 s in a targeted run. The pure carrier and slot tests
measured 0.2 and 0.1 ms alone after normalization, below 10 ms. The restart
case passed in 6.547 s through `scripts/test-tiers e2e`; its production drain
explains its tier. No budget exception was added.

The market harness now sets custody's enrollment upstream explicitly. Its
sync fixture retries within a bounded 30 s window while Phase 14's backoff is
active, retaining peer and result diagnostics on failure. Port selection binds
the same IPv4 loopback address as the endpoint. Shopping's borrowed-endpoint
reattachment fixture reports connectivity after reattachment to clear backoff.
These fixtures preserve production admission, deadlines and peer selection.

## Shared gates

The takeover verification on 2026-10-11 reran `./scripts/check`, which exited
0: clippy, 142 unit tests (124.3 ms wall and normalized) and 815 integration
tests (173.88 s wall and normalized), including the SDK without default
features. The tier runner's isolated reruns passed the enforced budgets; no
budget exception was added. The targeted e2e tier passed the restart case
in 14.326 s (15.73 s tier wall time). `./scripts/lint` exited 0.

`./examples/market-app/check` exited 0: six JVM tests, desktop and harness
compilation. `./examples/shoppinglist-app/check` exited 0: 24 JVM tests,
including `ApprovalsTest`, and desktop compilation. These are packaged JVM
runtime checks; they do not qualify phones, NAT or Internet relays.
The shopping run logged a Room background invalidation error during authority
replacement (`session authority changed; open a new session`); all 24 tests
still passed. This run does not establish background invalidation recovery.

`./examples/market-app/qualify` exited 0 on 2026-10-11, with all six scenarios
against three loopback voters on macOS 27.0.1 arm64, Java 25.0.4.1, 18 cores.
The measured scenarios took 137.611 s together:

| Scenario | Time | Result |
| --- | --- | --- |
| `last-item` | 17.621 s | Acceptance and typed rejection; rejection survives kill/reopen |
| `mesh` | 14.258 s | Neighbor-only callers discover the executor and read both outcomes |
| `loss` | 40.862 s | Lost reply and killed caller/executor recover the same ticket once |
| `takeover` | 11.239 s | Successor preserves the head and completes the pending ticket once |
| `refused-writes` | 7.490 s | DAO/SQL command-table writes refused; mergeable writes sync |
| `voters` | 46.141 s | Leader loss continues; quorum loss pauses; restore applies once |

Machine-readable host, build, voter count and notes are in
`local/market-qualification/evidence.json`. The first rerun overlapped the
shared check: its no-default-feature build replaced `target/debug/rocky-peer`
before the voter scenario started, causing custody to refuse iroh. Rebuilding
and rerunning the complete harness sequentially passed; no runtime or test
budget was changed. Run these gates sequentially when sharing a target directory.

The existing CI `market` job runs this same default selection; no hosted CI
run was triggered because this workspace must not be pushed. These results
qualify JVM processes on loopback, not phones, NAT or Internet relays.
