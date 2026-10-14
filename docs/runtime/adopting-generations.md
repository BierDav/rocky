# Adopting newer generations

A database is exactly one [generation](generations.md). A device moves its
database to any newer generation it meets, whatever schema its application
declares, and keeps syncing. [Decision 0068](../decisions/0068-adopting-newer-generations.md)
records why.

## Discovery

A sync finds a peer on a newer generation when the peer's credentials, a
frame or a [horizon statement](mergeable-horizon.md) name a manifest this
device does not know. The device then asks that peer for its generations on
the renewal protocol (`rocky.sdk.generations.v1`); every device answers, since
all of it is public. Before anything is saved it verifies the reply:

- every manifest is signed by the domain's publisher;
- each step's schema derives that step's manifest, and the offered schema the
  offered manifest;
- the transitions chain from step to step, through this device's generation,
  to the offered one.

A forged schema, manifest or transition is refused and nothing changes. An
issuer, the peer first when it is one, renews the device into the
generation, so its credentials and reader keys name the new manifest; a
device that reaches no issuer records `generation_pending` in its
[activity](activity.md) and tries again at the next sync.

## The switch

The renewed settings are saved first. The open replica then moves to the
generation with `EncryptedReplica::adopt_generation`: it verifies the lineage
again, activates the generation in one transaction and runs the
[backfill and switch](generations.md#plan-backfill-and-switch) before it
returns, so no frame of the generation applies before the switch. Frames the
replica refused under that generation apply when the peer offers them again.
Sessions and cursors end at the switch; table observers stay registered and
are told the tables were rebuilt. A crash after the settings were saved
reopens at the new generation and switches then; a crash during the plan
resumes it. The schema's `migrations` never run here: their results arrive
as signed writes.

## Applications behind and ahead

| The application declares | Opening does | Status `application` |
| --- | --- | --- |
| The installed schema | Opens it | `current` |
| What a generation the domain left defined | Opens the installed database unchanged | `behind` |
| A newer schema, on an issuer | Signs and adopts it, as [before](generations.md#plan-backfill-and-switch) | `current` |
| A newer schema, on a member | Fetches it from an issuer that signed it; otherwise opens at the domain's generation | `current` or `ahead` |

Schema versions are local and unsigned, so they never decide this. An
application behind or ahead of its database keeps reading and writing what
both schemas share; its statements on what differs fail with ordinary SQLite
errors. The [Room profile](sql-driver.md#room-284) answers `user_version` and
the schema pragmas of tables declared otherwise with what the application
declares, so a Room application of any version validates and opens.

## Joining

An enrollment carries the domain's current schema, which the joining device
installs whatever version of the same application it runs; pairing refuses
another application without consuming the invitation. The device then
reports `behind` or `ahead` as above.

## Baselines

A transition carries the time it was signed. Transitions that differ only in
that time make the same generation, so issuers that sign one schema apart
end on one manifest. Its baseline is the earliest signing time: a device
that meets a peer at its generation once compares their lineages and keeps
the earlier transition of each step, and `generations()` reports it.
Nothing is filtered by the baseline yet; [removal](../features_09/03-removal.md)
uses it for a re-added name, and with it a device that learns an earlier
baseline pulls again the frames stamped after it.

## Older generations

Frames of devices that have not migrated are admitted under their own
manifest. A replica that lacks the manifest of a generation a peer still writes
under, such as one it dropped, learns it on demand: when a sync meets a
frame or credentials it cannot place and the peer is not newer, the device
asks that peer for its generations, and `EncryptedReplica::learn_generations`
records each verified step whose transition leads to a generation it knows,
then admits the frames refused under it.

## Limits

Discovery does not adopt a [cut](generations.md#breaking-generations), which
drops the device's unsynced work: an application that declares the cut adopts
it at open, until [phase 6](../features_09/06-no-remnants.md) removes cuts.
Issuers that sign one schema apart each create reader keys for the tables it
adds. Removal is [phase 3](../features_09/03-removal.md).

## Evidence

- `rocky-secure` `tests::generations::an_open_replica_adopts_a_newer_generation_in_place`
  and `one_schema_signed_apart_is_one_generation_with_the_earliest_baseline`.
- `rocky-sdk` `tests/generations.rs`: `an_older_application_adopts_the_newer_generation_it_meets`,
  `a_crash_before_the_switch_resumes_at_the_newer_generation`,
  `a_newer_horizon_is_acknowledged_after_adopting_its_generation`,
  `an_older_application_joins_at_the_households_generation` and
  `issuers_that_sign_one_schema_apart_end_on_one_generation`.
- `rocky-sdk` `enrollment::tests::a_forged_generation_offer_is_refused_before_anything_changes`
  and `an_earlier_signed_step_moves_the_baseline`.
- `rocky-sdk` `tests/driver.rs` `an_older_room_application_validates_the_schema_it_declares`.
- `rocky-sdk` `an_older_generation_the_database_lacks_is_fetched_before_its_frames_apply`
  and `rocky-secure` `an_older_generation_the_replica_lacks_is_learned_before_its_frames_apply`.
