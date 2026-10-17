# 0068: Adopting newer generations and removal

Status: accepted and implemented
([phase 3](../features_09/03-removal.md)).
Date: 2026-10-14.

## Context

A device adopted a newer generation only when its application opened with
the newer schema. Until then it refused the newer manifest, reported
`upgrade_required` and held its own changes, and each transition carried an
admission horizon that refused devices two generations behind. A schema that
evolves for decades cannot depend on every device updating its application
([features_09](../features_09/README.md)).

## Decision

- **A database is one generation, whatever its application declares.** A
  device that meets a newer generation, by a frame, a credential or a
  horizon statement, fetches that peer's schema and publisher-signed
  lineage and verifies them: every manifest is the publisher's, each schema
  derives its manifest and the transitions chain through this device's
  generation. An issuer renews it into the generation; the settings are
  saved, then the open replica switches in place with the existing backfill
  and switch before any frame of that generation applies.
- **No admission horizon.** A transition carries a `cut` flag instead of a
  horizon; every generation of a lineage is admitted, except behind a cut
  until [phase 6](../features_09/06-no-remnants.md) removes cuts. Discovery
  does not adopt a cut, which would drop unsynced work.
- **A generation is its schema.** Transitions carry their signing time.
  Transitions that differ only in it make one generation, whose baseline is
  the earliest; every device keeps the earliest it learns. Pulling again the
  frames stamped after an earlier baseline comes with removal, the first
  change that ignores any frame; retained frames are replayed durably when
  a name is re-added or its baseline moves earlier.
- **Older manifests on demand.** A replica that lacks a generation a peer
  still writes under learns it from that peer's verified steps before its
  frames apply.
- **Applications may be behind or ahead.** An application that declares
  what a generation the domain left defined opens the installed database
  unchanged. A member whose application declares a newer schema no issuer
  signed, or that reaches none, opens at the domain's generation. Status
  reports `behind` or `ahead`; versions are local and never decide it.
  Room sees the version and tables its application declares.
- **Joining** installs the domain's current schema for any version of the
  same application.

- **Removal is replicated.** A valid new schema may remove mergeable tables
  and fields and their dependent constraints. The projection switch drops
  them on every adopter. Incoming writes verify their original authority
  before applying only kept fields. Checkpoint pages verify as signed before
  their stored rows are filtered. Names re-added under decision 0067 ignore
  writes stamped before the earliest signing baseline.
- **Added tables share their first reader key across issuers.** SDK signing
  derives it from the publisher secret, domain and table name under
  `rocky.sdk.generation-reader.v1`. This changes no protocol format.
  Revocation still rotates to random later epochs. Anyone holding the
  publisher key can derive these first keys; issuers already hold the other
  authority and secrets needed to issue credentials and deliver reader keys.

## Consequences

Transitions moved to `rocky.transition.v3`, signed at a time in microseconds,
the unit stamps compare with, and with the end of cuts to `rocky.transition.v4`
([decision 0070](0070-confirmed-rollouts-and-the-end-of-cuts.md)); others are
refused. Enrollments
carry the domain's schema, and pairing compares the application's name, not
its schema. Sessions end when a database switches; table observers stay.
Removal makes stamps hybrid time, so they compare with a baseline; the
encrypted file becomes `rocky.encrypted-replica.v16`, and other identities
are refused. An application schema that matches an earlier generation is
behind unless it declares a later version. See [removal](../runtime/removal.md) and
[generations](../runtime/adopting-generations.md).
