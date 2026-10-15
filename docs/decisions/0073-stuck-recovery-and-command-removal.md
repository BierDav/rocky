# 0073: Stuck recovery and removal of strong commands

Status: accepted for command removal; stuck recovery belongs to Phase 10.
Date: 2026-10-15.

## Context

[Coordinated writes](0072-coordinated-log-and-writes.md) order ordinary encrypted
row bundles. Keeping a second command system would retain immutable handler
versions, special tables, executor fencing and result histories for the same
application decisions.

## Decision

Phase 11 removes command descriptors, handler generations, command-controlled
and confirmed-read-only tables, preparations, results, tickets, receipt evidence
and forwarding. The coordinator retains its coordinated log and tenant control;
its control certificate lists trusted node keys, with no executor or invoker
roles. Native and Kotlin consumers use coordinated SQL. Shopping requests are
ordinary rows; trusted devices decide them in coordinated transactions, where
the first committed decision stands. Market restocking reads and updates stock
in one coordinated transaction.

Every changed format has one current encoding. Removed table classes, command
schema fields, old manifest/transition bytes, coordinator storage and link/file
identities are refused. Bootstrap signatures use `rocky.genesis.v2` and
trust bundles format 2; obsolete receipt-retention settings are removed. No migration or alternate reader is introduced.

The separate owner-signed recovery contract is specified by
[Phase 10](../features_09/10-stuck-recovery.md). This record does not implement
its cuts or claim that a permanently lost in-sync set can recover.

## Consequences

Applications own their business rules and coordination discipline on ordinary
rows. Being a trusted node grants no row access. Network/coordinator calls stay
outside storage transactions, and publication follows durable coordinated
commit. Development logs are process-local fixtures and lose their ledger at
process end; they are not durable services.

[Phase 11 evidence](../features_09/11-removing-strong-commands-evidence.md) records
verification and remaining acceptance work.
