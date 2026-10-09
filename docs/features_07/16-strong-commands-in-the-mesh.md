# 16. Strong commands in the mesh

Status: not started.
Owner: `rocky-sdk` command delivery and executor registration; the market
harness. Depends on [phase 9](09-discovery-and-partner-choice.md) and
[phase 15](15-market-example.md). Wave 4.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [horizon and strong commands](scope-mesh.md#6-horizon-and-strong-commands),
the [proposed defaults](scope-mesh.md#proposed-defaults),
[run and deliver](../runtime/typed-commands.md#run-and-deliver), its
[limits](../runtime/typed-commands.md#limits) and command delivery in
`crates/rocky-sdk/src/node.rs`.

A caller delivers a command to the executor among its configured peers and is
refused with "the domain's executor is not a configured peer" otherwise.
Executors are issuer devices. With roster-free admission a member may know
only a neighbor and a custody peer.

## Ordered slices

1. Audit the strong path for assumptions the mesh phases removed: a single
   enrolling issuer, a linear policy epoch in receipts, results or fences, a
   configured-peer list, one key epoch per scope. Record each and its fix.
2. Reaching the executor. The coordinator head names it; the caller needs a
   route. It asks admitted peers for that node's contact and dials it
   directly. Where no direct route exists, an admitted peer forwards the
   sealed command and the answer without reading either. The executor
   authenticates the caller's chain, not the forwarder's.
3. Tickets stay idempotent across routes: the same ticket delivered directly
   and through a forwarder runs one handler call.
4. Executor authority under merged revocations: a revoked executor's results
   are refused from the statement on, and confirmed results stay verifiable
   by their receipts.
5. Takeover with two issuers acting at once: both fence, the coordinator
   orders them, one executor continues at the same head.
6. Results reach a caller that is offline when the command confirms, through
   whichever admitted peer holds them.

## Completion gate

- [Acceptance 10](scope-mesh.md#acceptance) passes in the market harness with
  the caller holding contacts for a neighbor only.
- A forwarder cannot read, alter or replay a command; an altered one is
  refused by the executor.
- Direct and forwarded delivery of one ticket cause one handler call.
- Two issuers fencing concurrently leave one executor and no lost ticket.
- An executor revoked by an issuer other than the one that enrolled it stops
  confirming; pending tickets complete at its successor.
- Every market scenario of phase 15 still passes.
- [Typed commands](../runtime/typed-commands.md) describes delivery.

## Not in this phase

A scoped executor profile, executors on non-issuing devices and partitioned
command tables.
