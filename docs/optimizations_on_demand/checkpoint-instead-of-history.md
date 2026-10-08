# Checkpoint instead of history

Status: deferred. Catch-up is described in
[full and partial replicas](../runtime/replica-roles.md) and
[checkpoints](../runtime/checkpoints.md#rebootstrap).

## Trigger

Build this when replicas regularly come back far behind on scopes whose history
is much longer than their state, and catch-up time matters. A typical case is a
small, heavily edited scope after a long time offline. Another is a device that
subscribes to a scope with a long retained history.

## Current cost

A sender sends a checkpoint only when it must: when the peer is behind the
horizon and the operations it lacks are pruned. While the history still exists,
the sender always sends operations, however many there are compared with the
scope's rows.

Rows in a checkpoint are cheaper to transfer and install than individually
signed, encrypted operations. In the [phase 7 gate run](../runtime/scaling-limits.md#measured-limits):

- catching up 10,000 single-row operations took 48.7 s, about 205 operations/s;
- rebootstrapping a scope of about 35,000 rows took 5.8 s.

The comparison is not controlled. The catch-up ran while both sides kept
writing, and the rebootstrap did not. The trusted path has no measurement.

## Sketch

The sender already knows what the peer lacks: the peer's frontier arrives with
every range request. It also knows its scope's row count and the frontier of its
verified checkpoint.

1. Estimate the cost of history: the operations the peer lacks that the
   checkpoint covers. Estimate the cost of the checkpoint: the scope's rows, or
   bytes, with a measured factor.
2. When the history clearly costs more, answer the first range request with an
   advisory signal instead of frames: "a checkpoint is cheaper". It is distinct
   from `behind`, which stays mandatory.
3. The receiver decides. It may take the checkpoint through the existing
   rebootstrap, or decline and keep receiving operations. Reasons to decline
   include a metered link, a large set of unsent local edits, or a checkpoint
   that would not cover its own horizon.
4. After the import, catch-up continues with the operations after the
   checkpoint, as it does today.

On the trusted path, `catch_up` would return the same advice next to
`Catchup::BehindHorizon`. Tune the factor from measurements on both paths,
including a run without concurrent writes.

## Must not break

- A rebootstrap still verifies the checkpoint before switching, and reapplies or
  reports every unsent local operation.
- A checkpoint that ends before the receiver's own horizon is still refused; the
  receiver then falls back to operations.
- The advice never replaces the mandatory `behind` signal, and declining it
  always leaves a working operation catch-up.
- The rebootstrap's last step is one transaction proportional to the scope's
  rows. Advising it for large scopes must not push local write latency past its
  budget.
