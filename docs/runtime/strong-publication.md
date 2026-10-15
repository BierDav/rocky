# Coordinated publication

Publication of a decision uses a coordinated write of ordinary rows. The
application reads committed state and records its decision in the same SQL
transaction. A competing writer catches up and recomputes after a conflict.
Application rows, bundles and revision changes commit atomically; data and
notifications leave the device only after the coordinated entry commits.
The earlier trusted snapshot-command API has been removed.

Use [coordinated writes](coordinated-writes.md) for the supported contract,
APIs, failure handling and limits. [Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md)
tracks the remaining consumer conversions and acceptance gates.
