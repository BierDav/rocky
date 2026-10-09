# 15. Market example

Status: not started.
Owner: a new `examples/market-app`; its harness. Depends on
[phase 3](03-small-fixes.md). Wave 1.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [typed commands](../runtime/typed-commands.md), the market schema in
[getting started](../runtime/getting-started.md),
[the end-to-end evidence](../runtime/end-to-end-evidence.md), scenario 6 in
[the shopping audit](../runtime/shopping-qualification.md#acceptance-scenarios)
and [coordinator operations](../runtime/coordinator-operations.md).

The shopping example is mergeable-only. A Kotlin-only market application once
ran eight scenarios against three voters; it was removed, and that evidence
cannot be reproduced from the tree. Strong commands therefore have no
application-level gate. Restore a small one that the mesh and iOS phases can
run.

## Ordered slices

1. The application, with no Rust source, in the structure of the shopping
   example: a `shared` multiplatform module with the schema of the starter
   guide (`listings`, `memos`, `stock`, `holds`; `Reserve` and `Restock`),
   Kotlin handlers, Room DAOs reading the command-controlled tables and a
   keyed handler read; a desktop application and an Android application. Keep
   `shared` free of JVM-only dependencies so that
   [phase 18](18-shopping-and-market-on-ios.md) adds the iOS target only.
2. A minimal screen: listings, stock, a reserve action and the ticket's
   state, including pending and `outcome_unknown`.
3. A harness as for shopping: each device a separate JVM, three `rocky-voter`
   processes on loopback, a packaged custody peer. Scenarios:
   - two callers reserve the last item: one accepted, one typed rejection
     kept across kill and reopen;
   - a lost reply, a killed caller and a killed executor each complete the
     original ticket with one handler call;
   - the owner fences the executor and another continues at the same head;
   - DAO and SQL writes to command-controlled rows are refused;
   - leader loss continues, quorum loss pauses while mergeable writes sync.
4. The 4,096-command run of scenario 6, as JVM processes, with the record and
   byte budgets observed.
5. A check script and a CI job; the audit's scenario 6 names its command.

## Completion gate

- Every scenario has a reproducible command and recorded evidence.
- The application holds no Rust source and builds from packaged artifacts.
- The 4,096-command run completes, or stops at a named budget with the
  number reached.
- [The end-to-end evidence](../runtime/end-to-end-evidence.md) is
  reproducible from the tree again, or states what still is not.
- Scenario 6 leaves the audit's outstanding list for JVM processes.

## Not in this phase

Reaching an executor that is not a configured peer
([phase 16](16-strong-commands-in-the-mesh.md)), optimistic updates,
partitioned command tables and a public coordination service.
