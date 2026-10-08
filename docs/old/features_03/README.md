# SQL foundations and deferred Room integration

Status: phases 1–4 implemented with recorded limits; the former remaining
sequence is superseded as of 2026-10-06 by [architecture V4](../../proposals/architecture-proposal-v4.md)
and [features_04](../../features_04/README.md). Do not implement all nine old phases
as the next product milestone.

## Completed foundations

| Phase | Brief | Implemented boundary |
| --- | --- | --- |
| 1 | [Logical schema and entity identity](01-logical-schema-and-identity.md) | Typed logical descriptors, application keys and trusted row incarnations; automatic keys remain a blocker |
| 2 | [SQL transactions and capture](02-sql-transactions-and-capture.md) | Trusted managed statements, rollback/savepoints and durable transaction recovery |
| 3 | [Domains and placement](03-domains-and-placement.md) | Explicit bindings, one-file local atomic groups and offline placement migration; cross-file reads/writes blocked |
| 4 | [Authentication and encrypted bundles](04-authentication-and-encrypted-batches.md) | Secure sessions/SQL, scoped signed bundles and SDK enrollment; see its remaining items |

See [logical schemas](../../runtime/logical-schema.md), [SQL sessions](../../runtime/sql-sessions.md),
[database bindings](../../runtime/database-bindings.md), [secure sessions](../../runtime/secure-sessions.md)
and [secure SQL](../../runtime/secure-sql.md) for actual APIs. Completed milestones
are not proof that every original gate passed or trusted/secure profiles match.

## Where unfinished work goes

| Former brief | Current assignment |
| --- | --- |
| [5: Queries and observation](05-queries-and-observation.md) | Generic SQL/readiness/events: features_04 phase 4; Room catalogs/tracker: deferred adapter |
| [6: Migrations and recovery](06-migrations-and-recovery.md) | Generic generation/checkpoint/recovery: phase 8; Room import/callbacks: deferred adapter |
| [7: Strong state and effects](07-strong-state-and-effects.md) | Explicit typed commands/handlers: phase 5; transparent strong DAO writes: deferred |
| [8: Native SDK and lifecycle](08-native-sdk-and-lifecycle.md) | Composition/network/credentials: phases 1–3; language bindings/packages: phase 8 |
| [9: Compatibility and qualification](09-compatibility-and-qualification.md) | Generic fault/scale/service gates: phase 9; Room traces/driver: deferred adapter |

The original [research](research.md) records pinned Room/SQLite findings and is
not a current description of every source boundary. These retained briefs remain
useful references for an explicitly resumed Room integration; they do not require
Room to determine Rocky's schema, SDK ownership or release sequence.

## Optional Room acceptance contract

When that integration resumes, ordinary entities/DAOs should consume a qualified
Rocky-managed driver without repository-owned sync queues, projection mappings
or refresh calls. It must use the generic runtime/SDK contracts rather than
reintroduce networking or credential ownership into database construction.
Existing [projection adapter](../../runtime/room-integration.md) remains a separately
supported profile until replacement and import are qualified.

Ordinary CRUD is mergeable, not automatically strong. Missing keys/coverage are
availability errors. Local atomicity does not imply cross-file/distributed ACID.
Global uniqueness and business invariants need explicit commands. A narrower
adapter cannot claim compatibility with every existing Room application.
The former requirement for all nine gates is historical: the eventual adapter
selects its actual support profile and proves the corresponding compatibility
matrix before claiming it. No Room gate blocks generic services by itself.

The [scale budgets](../features_02/README.md#target-budgets) and existing misses
remain applicable. Future agents follow [the current execution contract](../../features_04/README.md#agent-execution-contract)
and update owning runtime guides when implementations change.
