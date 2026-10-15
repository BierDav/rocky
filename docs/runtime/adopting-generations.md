# Adopting newer generations

A database is exactly one [generation](generations.md). A device moves its
database to any newer generation it meets, whatever schema its application
declares, and keeps syncing. [Decision 0068](../decisions/0068-adopting-newer-generations.md)
records why.

## Discovery

A sync finds a peer on a newer generation when the peer's session hello, a
frame or a [horizon statement](mergeable-horizon.md) names a manifest this
device does not know, including when opening the link to the peer fails on
it; after adopting, earlier failures no longer back that peer off.
Credentials name no generation, so they never decide this. The device then
asks that peer for its generations on the renewal protocol
(`rocky.sdk.generations.v1`); every device answers, since all of it is
public. Before anything is saved it verifies the reply:

- every manifest is endorsed by the domain's owner or an issuer through its
  chain, and one met for the first time by a signer no statement this device
  holds ended: a revoked issuer moves nobody to a generation;
- each step's schema derives that step's manifest, and the offered schema the
  offered manifest;
- the transitions chain from step to step, through this device's generation,
  to the offered one.

A forged schema, manifest or transition is refused, nothing changes, and the
sync with that peer fails. The device adopts a verified generation on its
own: no issuer renews it, because its chain and reader keys hold in every
generation ([credentials](credentials.md)). A member that meets the
generation through another member, with no issuer reachable, adopts it the
same way and keeps writing every table its grants name. An issuer that
adopts one also derives the first reader key of each table it adds from the
registry key, as the signing issuer did, and certifies its node again under
the issuer authority it holds. Tables and columns the grants cover join the
projection at the switch.

A table the generation [grants like](generations.md#tables-granted-like-another)
one the device holds is not in its chain. The next sync asks an issuer, once
per generation, for an ordinary [renewal](sdk-enrollment.md), which grants it
with its reader key; until then the device cannot open that table. The
renewal grows the installed projection by the table through a
[projection plan](generations.md#plan-backfill-and-switch).

## The switch

The verified generation is saved first. The open replica then moves to the
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

Before dropping removed items, activation records incompatible never-offered
bundles as [held changes](held-changes.md). Kept-table effects are rebuilt
without them before the switch returns; status reports their count in `held`.

## Applications behind and ahead

| The application declares | Opening does | Status `application` |
| --- | --- | --- |
| The installed schema | Opens it | `current` |
| What a generation the domain left defined | Opens the installed database unchanged | `behind` |
| A newer schema, on an issuer | Signs and adopts additions; removals first require [confirmation](generations.md#confirmed-removals) | `current` after confirmation |
| A newer schema, on a member | Fetches it from a device that runs it, issuers first; otherwise opens at the domain's generation | `current` or `ahead` |

Schema versions are local and unsigned. They decide only one case: a schema
that matches an earlier generation is behind unless it declares a later
version than the installed one, which may add a removed name back
([removal](removal.md)). An
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
[Removal](removal.md) ignores writes to a re-added name stamped before it,
and a device that learns an earlier baseline pulls again the frames stamped
after it.

## Older generations

Frames of devices that have not migrated are admitted under their own
manifest. A replica that lacks the manifest of a generation a peer still writes
under, such as one it [dropped](generations.md#manifests-are-a-cache), learns
it on demand: when a sync, or a live link the peer opened, meets a frame or
a session hello naming a generation it cannot place and the peer is not
newer, the device asks that
peer for its generations. `EncryptedReplica::learn_generations` records each
endorsed manifest whose transitions, the peer's or its own, lead to a
generation it knows, then admits the frames refused under it; the SDK keeps
the manifest in its saved lineage again.

## Limits

Issuers that sign one schema apart derive the same first reader key for each
table it adds ([removal](removal.md#the-sdk)).

Issuers that sign different schemas on the same generation split the
lineage. A device adopts the branch it meets first and refuses the other as
"the offered generations do not include this device's"; nothing joins the
branches, and what a device applies of the other branch's writes is
undefined. This is an
[accepted limitation](../features_09/README.md#accepted-limitations).

## Evidence

- `rocky-secure` `tests::generations::an_open_replica_adopts_a_newer_generation_in_place`
  and `one_schema_signed_apart_is_one_generation_with_the_earliest_baseline`.
- `rocky-sdk` `tests/generations.rs`: `an_older_application_adopts_the_newer_generation_it_meets`,
  `a_crash_before_the_switch_resumes_at_the_newer_generation`,
  `a_newer_horizon_is_acknowledged_after_adopting_its_generation`,
  `an_older_application_joins_at_the_households_generation` and
  `issuers_that_sign_one_schema_apart_end_on_one_generation`.
- Adoption without an issuer: `rocky-sdk` `tests/generations.rs`
  `a_member_adopts_a_generation_from_another_member_with_no_issuer_reachable`,
  `an_invitation_created_before_a_schema_change_enrolls_a_device_after_it`
  and `a_table_granted_like_another_reaches_existing_members`.
- `rocky-sdk` `enrollment::tests::a_forged_generation_offer_is_refused_before_anything_changes`
  and `an_earlier_signed_step_moves_the_baseline`.
- `rocky-sdk` `tests/driver.rs` `an_older_room_application_validates_the_schema_it_declares`.
- `rocky-sdk` `an_older_generation_the_database_lacks_is_fetched_before_its_frames_apply`
  and `rocky-secure` `an_older_generation_the_replica_lacks_is_learned_before_its_frames_apply`.
- A dropped manifest learned again: `rocky-secure`
  `an_unreferenced_manifest_is_dropped_and_learned_again_from_a_frame` and
  `rocky-sdk` `a_manifest_nothing_retained_names_is_dropped_and_fetched_again`.
