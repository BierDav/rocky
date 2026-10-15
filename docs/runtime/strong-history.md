# Coordinated history

Coordinated writes retain ordinary row history under the encrypted runtime's
checkpoint and horizon policies. There is no separate command-result history,
receipt cache or strong horizon. The coordinator keeps bounded generic log
metadata; application nodes keep the bundles. See [encrypted checkpoints](encrypted-checkpoints.md)
and [the mergeable horizon](mergeable-horizon.md).

Use [coordinated writes](coordinated-writes.md) for the supported contract,
APIs, failure handling and limits. [Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md)
tracks the remaining consumer conversions and acceptance gates.
