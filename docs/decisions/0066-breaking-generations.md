# 0066: Breaking generations and tables granted like another

Status: accepted; cuts superseded by
[decision 0070](0070-confirmed-rollouts-and-the-end-of-cuts.md), which keeps
issuer-run migrations and `granted_like`.
Date: 2026-10-13.

## Context

Generations were additive only: rows of an earlier generation must stay
readable under the new one, so a new field had to be nullable and nothing
could compute values for existing rows. Applications such as an ERP require
new fields and derive them from existing data. Separately, grant profiles
name tables, so a member enrolled before a table existed never received it.

## Decision

- A **cut** is a transition whose `horizon` equals its new schema generation;
  `GenerationTransition::breaking` names it. Only a cut may require fields of
  existing tables, and each such field declares a default. The replica keeps
  fields by identity across a cut and admits no earlier generation.
- **Migrations run once, on one issuer.** The cutting issuer computes values
  with ordinary signed writes at an intermediate generation that holds the
  fields nullable, checks that no live row lacks one, and only then signs the
  cut. No replica transforms rows locally, so every device converges on the
  same signed values.
- **The cutting issuer's state is the base.** Every other device imports a
  checkpoint of the cut; work it had not delivered to that issuer is dropped.
  Frames behind a replica's horizon are neither served nor offered, and
  pseudonym keys are derived per cut, so dropped sequence numbers are never
  reused in an author log.
- **`granted_like`** maps a new table to the table whose grants it follows. It is
  part of the SDK schema, immutable once the table exists, and applied in one
  place, `Domain::effective`, wherever a profile is interpreted.

## Consequences

The schema JSON gains `granted_like` and `migrations`, so definitions move to
format 5 and format 4 definitions are refused. The descriptor they build is
unchanged and stays format 4, so installed databases still open. A cut drops data by design:
the application decides when that is acceptable. Two issuers cutting without
contact diverge; avoiding that needs coordination this record does not add.
Removal is implemented by [decision 0068](0068-adopting-newer-generations.md). See
[generations](../runtime/generations.md#required-fields).
