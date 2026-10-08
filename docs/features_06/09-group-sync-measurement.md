# 9. Group sync measurement

Status: not started.
Owner: `rocky-secure` measurement fixture; `rocky-sdk` sync. Depends on phases
[3](03-group-secrets.md), [5](05-partitioned-foreign-keys.md) and
[6](06-horizon-checkpoints-bootstrap.md). Wave 4.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the 200-partition budget](../runtime/partitioned-scopes.md#limits-and-budget)
and acceptance 6 of [the scope](scope-partitioned-constraints.md#acceptance).
The group secret makes key delivery independent of depth, but the SDK still
synchronizes scope by scope, with several round trips each, and keeps one log
and checkpoint per table and partition. A four-table group over 2,000
customers is 8,000 scopes per sync. This phase measures that cost and removes
it only if it exceeds a budget.

## Ordered slices

1. ERP fixture: 200 partitions; a group of four tables (customer, order,
   order line, attachment) with in-group references; order lines also
   reference an unpartitioned logged `products`.
2. Measure open, authoring, catch-up, an idle sync round, checkpointing,
   reopen and metadata per partition, for one table and for four. Report the
   cost per additional table.
3. Set the budget in this brief, next to the existing one.
4. If the idle round or catch-up exceeds it: exchange frontiers for all
   scopes of a group partition in one round trip. Stored data does not
   change; a wire change gets a decision record.

## Completion gate

- The measurement is reproducible with one documented command and recorded
  with host and build profile.
- The brief states the cost per additional table of a group and the budget.
- An idle sync of 200 partitions with four tables stays within the budget,
  with or without slice 4.
- No correctness test asserts a timing.
