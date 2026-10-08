# 16. Shopping application and qualification

Status: INCOMPLETE (2026-10-08): the required partitioned constraint schema is
refused by Kotlin and native installation. The version-2 shopping app now builds
for Android with Compose Multiplatform and passed one physical-phone LAN run;
that and the prerequisite checks are recorded in [the audit](../runtime/shopping-qualification.md).
Owner: `examples/shoppinglist-app` and the evidence guides. The market app named
in the original phase plan was removed on 2026-10-08. Depends on every earlier phase. Wave 5.
Follow the [execution contract](README.md#agent-execution-contract).

## Deliverable

The shopping-list application as a packaged Kotlin app with no Rust source:
Room DAOs on Rocky's file through the driver, a schema from the builder with
`lists` partitioned by its own `id` and `items` partitioned by `list`, per-list
sharing through partition grants and key envelopes, two Android devices and a
packaged always-on custody peer, a second issuing device by default. The
example stays mergeable-only and needs no coordinator
([decision 4](agreed-scope.md#decisions-of-2026-10-07)). The market app keeps
exercising strong commands with keyed handler reads, confirmed rows in Room
and pruned history.

## Acceptance scenarios

1. Two phones never online together share one list through the custody
   peer; a third device joins from a checkpoint and catches up.
2. A list is shared with one device and not another; the other never receives
   its frames. Revocation rotates the partition key.
3. An item is unlinked and relinked offline on two devices and converges; a
   duplicate title under the list's unique index converges by policy.
4. The creator phone is lost; the second device invites and renews.
5. A horizon drops tombstones on every device and a long-offline phone
   rebootstraps.
6. Market: 4,096 confirmed commands without hitting the budget; a DAO reads
   confirmed stock; a handler reads by key.
7. An ERP-shaped schema with a cross-scope foreign key and 200 partitions on
   one device, against the budget set in [phase 11](11-partitioned-scopes.md).

## Evidence and limits

Run the deterministic multi-process scenarios in CI and record one real
Android run with real network separately. Record measured limits in
[end-to-end evidence](../runtime/end-to-end-evidence.md) and update the
support matrix. Report what passed and what is outstanding; do not claim
Internet readiness from loopback.

## Verdict

The landed phases do not compose the required shopping schema: phase 11 still
refuses conflict policies and foreign keys on partitioned tables, and phase 14
keeps logged/support parents unpartitioned. Scenario 3 cannot start without a
partition-aware constraint implementation. The executable Kotlin preflight is
`./examples/shoppinglist-app/check-phase16-schema`; it fails before installation.
The [audit and gate results](../runtime/shopping-qualification.md) distinguish
prerequisite passes, Android qualification and outstanding acceptance scenarios.
No phase-16 completion is claimed.
