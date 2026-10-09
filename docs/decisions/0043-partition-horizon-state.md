# 0043: Partition horizon state and pending dependency index

Status: accepted.
Date: 2026-10-09.

## Context

[Phase 6](../features_06/06-horizon-checkpoints-bootstrap.md) finalizes and
rebuilds constraints within a group partition. Scanning every stored declaration
and checkpoint for every write makes admission cost grow with unrelated
partitions. A required participant must also be authorized for the concrete
parent scopes whose row cuts it acknowledges.

## Decision

Private replica storage uses `rocky.encrypted-replica.v7`, replacing v6 without
migration or an earlier reader. `secure_horizon_pending(dependency, scope)` has
a composite primary key and an index on `scope`. Each pending declaration indexes
its own scope and its related row/existence dependencies. Admission, replacement,
finalization and checkpoint adoption maintain the index in their existing atomic
transactions. Writes and incoming frames look up only affected declarations.
Finalized declarations remain stored for signature, sequence and custody checks,
but leave the pending index.

Checkpoint format 6 is unchanged. Its authenticated partition tuple, materialized
rows and per-scope identity marks already contain the necessary partition state.
Candidates, visible/hidden marks and support rows remain derived local state;
none is added to the wire. Rebuild selects the concrete group partition, or the
connected constraint graph when an unpartitioned existence base changes.
Unrelated constraint graphs remain untouched.

Finalization selects only real projection rows (`row_kind = 0`) whenever the
logical table has that column, including unlogged support parents. Register
stamps and identity marks survive. Required peers must have whole-scope read or
custody authorization for every related parent row cut as well as the finalized
scope. Actual acknowledgments still prove exact cuts and local parent holdings.

## Consequences

Installing this private format requires reinstalling earlier replica files.
There is one accepted file identity and one accepted checkpoint version.
A horizon in A neither seals B nor enrolls a B-only reader as an A participant.
Finalization remains one transaction proportional to the affected scope's rows;
all intermediate mutations roll back on interruption. Checkpoint import and
rebootstrap rebuild the affected group partition without rewriting another
partition's rows, identity marks or derived hidden state.
