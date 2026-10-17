# Following schema changes

A device switches schema when the bookmark it follows reaches a tip with
another schema, never because a peer runs one. Publishing is in
[schema changes](generations.md); [decision 0075](../decisions/0075-generations-as-sealed-forks.md)
records the model.

## Pending switches

Receiving tip statements never fails because a schema is unknown. When the
followed bookmark moves to a tip whose schema is not installed, the import
records a pending switch (`pending` in `generation()`); the workspace stays
read-only at its tip until the switch.

## The schema catalog

The replica keeps `secure_tip_schemas`: per manifest, its canonical
definition and the adapter's record. A record is the language-neutral SDK
schema without `migrations`, plus its `generation`; the SDK verifies it by
deriving the manifest with the domain's publisher, mesh and domain and
comparing the hash with the commitment the graph names, and the replica
checks the commitment against its own requirement again.

`EncryptedReplica::required_schemas()` names exactly the schemas a device
needs:

- the followed bookmark's target;
- every tip on the canonical path from the oldest tip that still holds a
  stored row, retained frame, replay base or base being imported, to the
  target, since the [path filter](removal.md#the-path-filter) reads each;
- the tips of kept seals and held changes.

Unfollowed branches never appear in it. The SDK first derives each
requirement from its own declared schema; for the others it asks routed
contacts, issuers first, over `rocky.sdk.tip-schema.v1` (`{manifest}` →
record or nothing). Any eligible contact may answer, whatever tip it
follows; its answer is accepted only once it derives exactly the requested
commitment and the replica still requires it. A pass dials at most 16
providers within 30 seconds and retries with the usual backoff; absence,
silence or a malformed record moves on to another provider. A device serves
exact lookups from its catalog and never fetches on another's behalf.

## The switch

With every schema on the path installed, the SDK saves the intent
(`Settings.pending`), calls `switch_schema` and then saves its settings. A
crash after the replica committed reopens at the new schema from the
replica's installed tip record; a crash before it resumes on the next pass.
A sync pass that switched the followed tip syncs the new tip once more. A
registered line node is certified again for the tables the new schema adds.

## Applications behind and ahead

An application older than its database is `behind`: it keeps reading and
writing what both schemas share, and its statements on what differs fail
with ordinary SQLite errors. The [Room profile](sql-driver.md#room-284)
answers `user_version` and the schema pragmas of tables declared otherwise
with what the application declares. A member whose application is newer
waits `ahead` with `waiting_for_issuer` until its bookmark reaches the
schema.

## Joining

An enrollment carries the inviter's graph statements, its followed bookmark
and that tip's schema record. The joiner verifies them, installs that schema
and follows that bookmark, whatever version of the same application it runs;
a scope its peer offers no base for joins by frames.

## Evidence

- `rocky-secure` `tests::tip_schemas` (pending switch read-only across
  reopen) and `tests::generations::switches`.
- `rocky-sdk` `tests/generations.rs`
  `a_member_fetches_a_required_schema_from_another_member_with_no_issuer_reachable`,
  `a_device_on_main_never_fetches_or_installs_a_dev_branch_schema` and
  `a_crash_on_either_side_of_the_switch_resumes_on_the_followed_schema`.
- `rocky-sdk` `enrollment::tests::forged_tip_schema_records_are_refused_without_changing_the_installation`.
- `rocky-sdk` `tests/driver.rs` `an_older_room_application_validates_the_schema_it_declares`.
