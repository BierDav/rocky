# Recovery of coordinated rows

Application decisions are ordinary rows published by
[coordinated writes](coordinated-writes.md). The removed command runtime's
preparations, result archives, acknowledged-loss domain replacement and managed
command effects are no longer APIs.

Use [backup and restore](backup-and-restore.md) for the supported database backup
contract and [encrypted checkpoints](encrypted-checkpoints.md) for row history.
An uncertain coordinated entry remains unsent until the runtime resolves it;
an aborted one is voided and its local projection rebuilt.

## Stuck coordinated domains

A trusted node that lost committed entries can rebuild from a member's complete
retained bundles, using the [coordinated catch-up path](coordinated-writes.md#catch-up).
The coordinator verifies metadata only; its genesis anchor and exact retained
entry hashes let the node verify the original frames before rejoining. Missing
entries or a gap outside the hash window keep appends refused.

What no device holds is abandoned only by the owner's recovery seal of the
stuck tip: every device holds the abandoned writes for their authors and
rolls their effects back. See [stuck recovery](stuck-recovery.md).

## External effects

Applications own external effects and their delivery/idempotency policy; a local
row commit does not prove an external action occurred.
