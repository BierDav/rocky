# 16. Strong commands in the mesh

Status: complete locally on 2026-10-11; hosted CI not run. See [evidence](16-strong-commands-evidence.md).
Owner: `rocky-sdk` command delivery and executor registration; the command
protocol in `rocky-core` and `rocky-secure`; the market harness. Depends on [phase 8](08-concurrent-rotation.md),
[phase 14](14-contacts-and-partner-choice.md) and
[phase 19](19-market-example.md). Decision record 0059. Wave 5.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [horizon and strong commands](scope-mesh.md#6-horizon-and-strong-commands),
what was [decided](scope-mesh.md#decided-2026-10-09),
[run and deliver](../runtime/typed-commands.md#run-and-deliver), its
[limits](../runtime/typed-commands.md#limits), and `run` and `results` in
`crates/rocky-sdk/src/node.rs` and `sync.rs`.

`run` reads the coordinator head, which names the executor by a 16-byte node
ID. If that is another device, `run` looks for it among the configured peers
and refuses with "the domain's executor is not a configured peer" when it is
not one. Executors are issuer devices. With roster-free admission a member
may know only a neighbor and a custody peer.

## Settled

- A caller finds the executor by asking admitted peers for the contact of
  the node ID the head names, through the contact request of phase 14, then
  dials it.
- Where no direct route to the executor exists, an admitted peer forwards
  the command and its answer. The forwarder passes on the caller's signed
  `EncryptedCommand` unchanged; the executor authenticates the caller's
  chain, never the forwarder's, and the caller verifies the answer as if it
  had dialed. A forwarder can neither alter nor replay a command; what it
  can read is what the envelope already shows a relay, and the record states
  it.
- A direct route is tried first. Forwarding goes one hop: a forwarder
  delivers to the executor itself and never to another forwarder.
- A forwarder serves at most 8 commands at a time per caller and holds
  nothing after the answer or a timeout; it keeps no queue for an offline
  executor.
- Forwarding replaces `rocky/command/1` by the next command protocol under
  decision record 0059. Tickets, receipts, results and command signatures
  keep their formats.
- An application that needs its own carrier keeps `command_envelope`,
  `command_execute` and `command_settle`.
- Results already reach a caller that was offline through any upstream that
  holds them. This phase verifies that and builds nothing for it.
- The shopping example's approvals are a second strong-command application
  for the audit.

## Ordered slices

1. Audit the strong path, in the market and in shopping's approvals, for
   assumptions earlier phases removed: a single enrolling issuer, a policy
   epoch in command verification, receipts, results or fences, a
   configured-peer check on either side, one key epoch per scope. Record
   each finding and repair it inside the existing formats.
2. Reaching the executor: `run` takes the executor's contact from the local
   list, configured or learned. Without one it asks the admitted peers of
   the current pass, checks that the answered node key hashes to the head's
   node ID, keeps the contact and dials. The old refusal text is removed.
3. Forwarding, under decision record 0059: when the dial fails or no asked
   peer knows a contact, `run` hands the command to an admitted peer of the
   current pass that reaches the executor, and receives the answer through
   it. A peer that does not reach the executor says so and the caller tries
   the next. While none does, the ticket stays unresolved and `run` returns
   a retryable error that says so.
4. Admission at the executor: a caller is admitted by its chain and the
   revocation set, not by being a configured peer, whether the command came
   directly or forwarded. A command from a device in the set is refused with
   a non-retryable error, and a forwarder in the set is refused as a peer.
5. Tickets stay idempotent across routes: one ticket delivered directly and
   through a forwarder, and again after a lost reply, causes one handler
   call.
6. Executor authority under the revocation set: a revoked executor's results
   are refused from the statement on, and results confirmed before stay
   verifiable by their receipts.
7. Takeover with two issuers fencing at once: the coordinator orders them
   and one executor continues at the same head. Verify this first; repair
   only what `rocky-sdk` owns.
8. Results for an offline caller: add the check that a ticket confirmed
   while its caller was offline settles through a peer other than the
   executor.

## Completion gate

- [Acceptance 10](scope-mesh.md#acceptance) is a CI run in the market
  harness: the caller holds contacts for a neighbor only, submits a command
  and reads its outcome.
- Two callers that never met the executor reserve the last item: one is
  accepted and one receives the typed rejection.
- The answered contact of another node is refused by the node ID check.
- A caller with no route to the executor, the relay disabled, submits
  through a forwarder and reads the outcome.
- A forwarder that alters one byte is refused by the executor; a forwarder
  replaying a delivered command causes no second handler call; an altered
  answer is refused by the caller.
- One ticket delivered directly and through a forwarder causes one handler
  call.
- A ninth concurrent command of one caller is refused by the forwarder with
  a retryable error; a forwarder restarted mid-command holds nothing and the
  caller's retry completes the ticket.
- A build with the earlier command protocol is refused.
- A CI run shows a revoked caller's command refused at the executor, and an
  executor revoked by an issuer other than the one that enrolled it
  confirming nothing more; its pending tickets complete at the successor.
- Two issuers fencing concurrently leave one executor and no lost ticket.
- A caller offline at confirmation reads the outcome after syncing with the
  custody peer alone.
- Every market scenario of phase 19 and shopping's approval checks pass.
- [Typed commands](../runtime/typed-commands.md#run-and-deliver) describes
  how the executor is found, forwarding and what a forwarder learns, and no
  longer requires a configured peer.

## Not in this phase

- Forwarding over more than one hop, or a forwarder queuing a command for
  an executor it cannot reach.
- Hiding from a forwarder who calls whom, or sizes and timing.
- Changing the coordinator protocol, command signatures, tickets, receipts
  or results.
- A new session message: the contact request is phase 14's.
- Pushing results to a caller.
- A scoped executor profile and executors on non-issuing devices.
- Partitioned command tables.
- Raising the command budget of 4,096 records and 32 MiB.
