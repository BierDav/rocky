# Optimizations on demand

Status: proposed. Nothing here is implemented.

Each document describes an optimization that was considered and deliberately
deferred, because the current design is correct and its cost is acceptable for
the workloads measured so far. Build one only when a real workload hits the cost
it removes, and record the decision when you do.

Every entry states the trigger that justifies the work, the current cost, a
sketch of the change, and what it must not break.

| Optimization | Trigger |
| --- | --- |
| [Lean checkpoints](lean-checkpoints.md) | The doubled storage or write cost of checkpoint tables matters for an application |
| [Checkpoint instead of history](checkpoint-instead-of-history.md) | Catch-up of far-behind replicas is slow on scopes whose history is much longer than their state |
| [Single-copy typed rows](single-copy-typed-rows.md) | The second copy of mergeable values in a typed-table application costs too much storage or write time |
| [Cloned node detection](cloned-node-detection.md) | Devices or servers are copied while running, and their copies keep writing |
| [Coordinated migrations](coordinated-migrations.md) | Issuers often migrate the same generation concurrently and the duplicate work matters |

The former command-specific proposals are historical after
[command removal](../features_09/11-removing-strong-commands.md). Race-free
application transitions now use [coordinated writes](../runtime/coordinated-writes.md).
