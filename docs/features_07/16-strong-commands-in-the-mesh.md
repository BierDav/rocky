# 16. Strong commands in the mesh

Status: not started.
Owner: `rocky-sdk` command delivery and executor registration; the market
harness. Depends on [phase 8](08-concurrent-rotation.md),
[phase 14](14-contacts-and-partner-choice.md) and
[phase 19](19-market-example.md). Wave 5.
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
- Nothing is forwarded. `rocky/command/1`, tickets, receipts and results
  keep their formats; this phase takes no decision record. A change it
  cannot avoid is reported as a blocking contract, not made.
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
   node ID, keeps the contact and dials. While no asked peer knows it, the
   ticket stays unresolved and `run` returns a retryable error that says so;
   the old refusal text is removed.
3. Admission at the executor: a caller is admitted by its chain and the
   revocation set, not by being a configured peer. A command from a device
   in the set is refused with a non-retryable error.
4. Tickets stay idempotent across routes: one ticket delivered over two
   routes, and again after a lost reply, causes one handler call.
5. Executor authority under the revocation set: a revoked executor's results
   are refused from the statement on, and results confirmed before stay
   verifiable by their receipts.
6. Takeover with two issuers fencing at once: the coordinator orders them
   and one executor continues at the same head. Verify this first; repair
   only what `rocky-sdk` owns.
7. Results for an offline caller: add the check that a ticket confirmed
   while its caller was offline settles through a peer other than the
   executor.

## Completion gate

- [Acceptance 10](scope-mesh.md#acceptance) is a CI run in the market
  harness: the caller holds contacts for a neighbor only, submits a command
  and reads its outcome.
- Two callers that never met the executor reserve the last item: one is
  accepted and one receives the typed rejection.
- The answered contact of another node is refused by the node ID check.
- One ticket delivered over two routes causes one handler call.
- A CI run shows a revoked caller's command refused at the executor, and an
  executor revoked by an issuer other than the one that enrolled it
  confirming nothing more; its pending tickets complete at the successor.
- Two issuers fencing concurrently leave one executor and no lost ticket.
- A caller offline at confirmation reads the outcome after syncing with the
  custody peer alone.
- Every market scenario of phase 19 and shopping's approval checks pass.
- [Typed commands](../runtime/typed-commands.md#run-and-deliver) describes
  how the executor is found and no longer requires a configured peer.

## Not in this phase

- Forwarding a command or its answer through another peer, in any form.
- Changing the command protocol, the coordinator protocol or any command
  format.
- A new session message: the contact request is phase 14's.
- Pushing results to a caller.
- A scoped executor profile and executors on non-issuing devices.
- Partitioned command tables.
- Raising the command budget of 4,096 records and 32 MiB.
