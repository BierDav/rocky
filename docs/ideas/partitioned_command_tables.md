# Partitioned command tables

Status: idea; not scheduled.

## Background

A command's tables are unpartitioned: the executor holds them completely, every result carries
them whole, and one sequence per domain orders every command. Mergeable tables have
[partitioned scopes](../runtime/partitioned-scopes.md) with one key per partition.

## Problem

An ERP's strong invariants are often per partition: stock per warehouse,
document numbers per fiscal year and company, postings per ledger. With
unpartitioned command tables, an executor must hold and decrypt every
warehouse's stock to reserve one item, every device that verifies a result
reads every partition, and one sequence serializes unrelated warehouses.

## Idea

Let a command declare the partitions it touches and run against those alone.

- A command's read and write set names partition tuples. The executor needs
  the keys of those partitions only, and a device verifies the parts of a
  result it holds keys for, as it does for scopes today.
- The confirmed rows per partition are sealed under that partition's key, so
  a result reveals nothing about partitions a reader does not hold.
- Ordering stays per domain at first. Per-partition sequences are the
  [independent command sequences](../optimizations_on_demand/independent-command-sequences.md)
  optimization, and depend on this.
- Pruning of strong history stays per domain
  ([strong history](../runtime/strong-history.md)),
  so this changes neither the horizon nor the strong base.

## Why it waits

It changes the handler contract, the result format and which device may
execute. [Large command tables](../optimizations_on_demand/large-command-tables.md)
already plans results as row changes with keyed reads; partitioning is best
designed together with that, once an application needs it.

## Open

- Who may execute a command whose partitions no single device holds: refuse,
  or split the command.
- Whether a command may read a partition it cannot write, and how that read
  is recorded in its read set.
- How the coordinator learns which executor is eligible for which partitions
  without learning the partition values.
