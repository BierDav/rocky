# Independent command sequences in one schema

Status: deferred. The current contract is described in
[typed commands](../runtime/typed-commands.md#declare-commands).

## Trigger

Build this when unrelated strong commands of one application delay each other,
or when one slow or unavailable executor stops strong progress for tables it
has nothing to do with. Example: an ERP whose stock reservations and document
numbering share one schema but no invariant.

## Current cost

Every command of a schema declares the same `resources`, and one confirmed
sequence orders all of them.

- Commands run one at a time per database, whatever tables they touch.
- One executor is assigned for the whole domain. Its credentials need
  `Execute` on every command-controlled table.
- A device must hold the complete confirmed state of every table to execute
  any command.

Applications can already separate invariants by placing them in different
domains. That costs a second domain identity, enrollment and coordinator
registration, and rows of the two domains cannot share one reader scope.

## Sketch

Let a schema declare several resource groups. Each group is a set of
command-controlled tables with its own confirmed sequence, coordinator head
and executor assignment.

1. A command names exactly one group. Its handler reads and writes only that
   group's tables.
2. The coordinator orders each group independently. Fencing, retirement and
   receipts are per group.
3. Execution rights and complete-state requirements apply per group, so a
   device can execute one group while holding none of another.
4. Tickets and results keep their identities; their sequence numbers become
   per group.

This changes the signed command definition, the coordinator's per-domain
state and the client's receipt evidence. It needs a decision record and a
generation transition for existing domains.

## Must not break

- A table belongs to one group. Two sequences never write the same row.
- No command spans groups. A workflow over two groups stays two commands with
  their own outcomes, as two domains are today.
- Coordinator storage must stay bounded by live domains, groups and sessions,
  as [bounded retention](../runtime/coordinator-retention.md) requires.
- A reader of one table must still verify and apply its part alone.
