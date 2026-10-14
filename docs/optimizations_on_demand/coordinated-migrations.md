# Coordinated migrations

Status: deferred. The current contract is described in
[migrations at any size](../features_09/07-migrations-at-any-size.md#settled).

## Trigger

Build this when issuers of one household often migrate the same generation
concurrently and the duplicate work matters: large migrations run and
published twice, or measurable sync traffic from the second publication.

## Current cost

Issuers that cannot reach each other may each run a generation's migrations.
Deterministic `insert` ids and latest-wins updates make both results converge,
so no data is duplicated, but each issuer runs the migration and publishes
every changed row. Receivers apply both publications.

## Sketch

When the domain has a reachable coordinator, an issuer claims "migrating
generation G" as a [coordinated write](../features_09/09-coordinated-writes.md)
before it runs the migrations. Another issuer that finds the claim committed
waits for the published rows instead of migrating.

1. The claim is an optimization only: without a reachable coordinator, the
   issuer migrates as today.
2. A claim whose migration never publishes expires, and another issuer
   migrates.

## Must not break

- Schema upgrades never require a coordinator.
- Concurrent migrations still converge without one.
- A claim never delays a migration longer than its expiry.
