# Lean checkpoints

Status: deferred. The current design is confirmed in
[design rationale](../runtime/checkpoints.md#design-rationale).

## Trigger

Build this when an application finds the cost of the checkpoint copy too high
after it has reduced `checkpoint_tables()` to the tables a merge needs. Examples:
a file that must stay under a device storage quota, or a write rate that the
second application of each operation cannot sustain.

## Current cost

Each domain keeps a checkpoint copy of the declared tables, with their indexes,
in the same file. Retained operations are replayed into it in bounded steps.

- Storage for the declared tables and their indexes doubles.
- Every applied operation is applied a second time, later, by the replay.
- Installing an application creates its tables twice: a 2,000-table schema
  installed in 8.5 s instead of 4.8 s.

Measured at 2,000 tables, writes cost 1.16× and reopen took 117 ms.

## Sketch

An opt-in mode, per application or per table, that keeps only what the copy is
needed for:

- **Commitment plus rebuild on demand.** Keep the running row digest of the
  projection, not a copy. A checkpoint is then the digest at a frontier. An
  export reads the projection in one long read snapshot instead of a stable
  copy. A strong cut behind the projection is rebuilt only from operations
  after a previous full checkpoint, so full checkpoints are still taken, but
  rarely.
- **Copy only on export.** Materialize the copy when a peer needs a rebootstrap
  and drop it after the transfer, trading export latency for steady-state cost.

The [checkpoint design](../runtime/checkpoints.md#design-rationale) rejected a commitment without rows as the default: it cannot be
recomputed from the previous checkpoint, a cut behind the frontier cannot be
rebuilt, and an export must hold one read snapshot for the whole transfer. A
lean mode must state which of those it gives up, and for which tables.

## Must not break

- The audit must still detect a projection that differs from its history.
- Rebootstrap must still import a verified state and reapply local edits.
- A strong request must still execute at its sealed cut, or stay visibly held.
- Two replicas at one frontier must still agree on the checkpoint record.
