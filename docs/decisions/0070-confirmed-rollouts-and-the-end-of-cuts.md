# 0070: Confirmed rollouts and the end of cuts

Status: accepted; the end of cuts implemented
([phase 6](../features_09/06-no-remnants.md)), confirmed rollouts pending
([phase 5](../features_09/05-confirmed-rollouts.md)).
Date: 2026-10-14.

## Context

[Decision 0066](0066-breaking-generations.md) let a generation require fields
of existing tables only as a *cut*: no earlier generation was admitted, every
other device imported the cutting issuer's checkpoint and its unsynced work was
dropped. Once devices adopt any newer generation they meet
([decision 0068](0068-adopting-newer-generations.md)), the cut no longer
protects anything. And every device kept the manifest of every generation that
ever wrote, so a schema evolving for decades accumulated state
([features_09](../features_09/README.md)).

## Decision

- **No cuts.** A required field with a default, new or once nullable, is an
  additive change. Rows that lack it, or hold NULL in it, read the default on
  every device, including rows an older generation writes later; the
  projection switch rewrites stored NULLs the same way. The first issuer runs
  the schema's `migrations` once as ordinary signed writes, between a step that
  holds the fields nullable and the generation that requires them. Pseudonym
  keys no longer change per generation, and nothing is re-sealed or
  rebootstrapped.
- **Transitions are the lineage; manifests are a cache.** A transition carries
  the field ids rows of its earlier generation may lack, and the table and
  field ids it names and removes, so no device needs an earlier schema to read
  old rows or to know what a removal dropped. Where both manifests of a step
  are held, a device checks the transition against them. Every device keeps the transitions. It drops
  an earlier generation's manifest once no retained frame, checkpoint, horizon
  statement, existence base, command or credential chain of a retained frame or
  registered author names it, and fetches it again from a peer that writes
  under it. Chains a peer last authenticated with do not keep a manifest: the
  peer presents its chain again. A recorded generation stays verifiable by its
  signed transition; admitting a new frame of it needs its manifest.

## Consequences

`SchemaChange::Breaking` and the transition's cut flag are gone; transitions
move to `rocky.transition.v4`, the encrypted replica to
`rocky.encrypted-replica.v16`, and a saved lineage step holds its manifest only
while the replica does. Other versions are refused. Decision 0066 stands for
`granted_like` and issuer-run migrations; its cuts are superseded. Dropping
follows horizon finalization, so a device keeps an earlier manifest as long as
retention keeps that generation's frames. See
[generations](../runtime/generations.md#required-fields).
