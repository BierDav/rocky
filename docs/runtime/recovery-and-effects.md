# Recovery of coordinated rows

Application decisions are ordinary rows published by
[coordinated writes](coordinated-writes.md). The removed command runtime's
preparations, result archives, acknowledged-loss domain replacement and managed
command effects are no longer APIs.

Use [backup and restore](backup-and-restore.md) for the supported database backup
contract and [encrypted checkpoints](encrypted-checkpoints.md) for row history.
An uncertain coordinated entry remains unsent until the runtime resolves it;
an aborted one is voided and its local projection rebuilt.

A permanently lost in-sync set, reconstruction from member-held bundles and
owner-signed recovery cuts are specified by
[Phase 10](../features_09/10-stuck-recovery.md). They are not implemented by
[command removal](../features_09/11-removing-strong-commands-evidence.md).
Applications own external effects and their delivery/idempotency policy; a local
row commit does not prove an external action occurred.
