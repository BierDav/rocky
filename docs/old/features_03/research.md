# Source findings and design options

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: research, 2026-10-05. Recommendations are proposals; no driver or new
transaction protocol was implemented during this research.

## Repository observations

| Source boundary | Observed behavior | Consequence |
| --- | --- | --- |
| `rocky-sdk` register application | One `sdk_rows` table stores collection/value/register JSON; entities use creating operation IDs | Room's typed columns and application keys need a different schema/identity contract |
| `runtime::sdk::Session` / Kotlin `RuntimePort` | Single-domain reads and individual mutation calls; no SQL connection/savepoint API | Wrapping this port cannot preserve ordinary multi-statement Room transactions |
| `secure::EncryptedReplica` authoring | Exact before/after row batches; 32 rows; one resource scope; stable handles | Reuse security/recovery machinery, extend its transaction boundary deliberately |
| `ResourceScope` / `RowChange` | Scope binds domain, encryption and table; grant checks pair row and changed field | Multi-table transactions and unchanged assigned fields require explicit semantics |
| Scoped connections | Logical-to-physical rewriting, deterministic domain hash prefixes | Existing mapping is useful, but is not a complete arbitrary-SQL resolver |
| Private SQLite adapter | REAL values, safe preupdate callback, row-digest tracking, read authorizer | Reuse the engine and multiplex its existing hooks; public codecs still lack REAL |
| Trusted placement | 4,096 domains, 256 file slots; no cross-file transaction | Do not imply one logical connection can atomically write every file |
| Kotlin desktop integration | Separate Room projection file, explicit writes and durable queue | Current supported path remains until the replacement qualifies |
| Secure replica APIs | Signed authentication, scoped snapshots and commands, partial replicas and recovery | None is automatically inherited by the trusted Kotlin SDK |

Relevant code: [current typed application](../../../crates/rocky-sdk/src/typed.rs),
[SDK session](../../../crates/rocky-runtime/src/sdk.rs),
[secure authoring](../../../crates/rocky-secure/src/replica/author.rs),
[permissions](../../../crates/rocky-core/src/permissions.rs),
[SQLite adapter](../../../crates/rocky-storage/src/sqlite.rs),
[namespaces](../../../crates/rocky-storage/src/scoped_connection.rs).
For security/recovery limits use the newer focused guides, particularly
[scaling and rebuilds](../../runtime/scaling-and-rebuilds.md) and
[replica roles](../../runtime/replica-roles.md); some older security-guide quota
paragraphs predate checkpoint and pruning support.

## Exact Room version inspected

The project pins Room runtime/compiler 2.8.4 and SQLite bundled 2.6.2. The
[official Room 2.8.4 source archive](https://dl.google.com/dl/android/maven2/androidx/room/room-runtime/2.8.4/room-runtime-2.8.4-sources.jar)
was downloaded and inspected in memory. Findings:

- `RoomConnectionManager` reads/writes `user_version`, manages the identity table,
  configures connection PRAGMAs and queries the schema catalog.
- `SchemaInfoUtil` reads table, foreign-key and index PRAGMAs, including index
  extended metadata and stored view SQL.
- `ConnectionPoolImpl` uses read-only connections, transaction variants and
  nested SQL savepoints.
- `InvalidationTracker` creates a modification log and table-level AFTER
  triggers; its log is temporary by default and belongs to the connection.

The public [SQLite driver contract](https://developer.android.com/reference/androidx/sqlite/SQLiteDriver)
and [Room configuration](https://developer.android.com/kotlin/multiplatform/room)
provide the extension point. Newer Room versions are not evidence for 2.8.4;
maintain a pinned compatibility fixture before changing versions.

## SQLite mechanisms evaluated

| Mechanism | Useful property | Limitation / proposed disposition |
| --- | --- | --- |
| Real typed tables with runtime name resolution | Native SQL planning, indexes, constraints and metadata | Preferred foundation; resolution must understand SQL scopes and permission views |
| TEMP views over attached physical tables | Simple logical SELECT names | Insufficient as a table compatibility strategy |
| Virtual tables | Custom row access and planning | No ordinary triggers; cannot transparently inherit Room's tracker |
| Preupdate capture | Actual old/new row images, including generated ordinary-table writes | Preferred experiment; multiplex digest capture and implement rollback accounting |
| Session extension | Coalesced primary-key changesets | Evaluate only as an alternative; net changes omit write intent and incarnation transitions |
| Bare update hooks or SQL string matching | Easy notification or apparent statement routing | Insufficient for authorization, exact row capture and transaction recovery |
| `ATTACH` | SQL queries across a bounded set of files | Not an arbitrary-file query engine or WAL atomic-commit solution |

SQLite's [preupdate hook](https://www.sqlite.org/c3ref/preupdate_blobwrite.html)
provides row images for ordinary tables. Only one callback may be installed per
connection; capture must coexist with Rocky's digest tracker. It does not cover
virtual/system tables. Never treat a callback as a commit notification.

The [session extension](https://www.sqlite.org/sessionintro.html) requires a
declared primary key, excludes NULL-key rows and virtual tables, and coalesces
intermediate changes. Its one-hook integration and rollback behavior must be
tested if selected. A changeset is a capture format, not Rocky's authorization or
convergence protocol; applying it directly is not a replacement reducer.

[SQLite authorization](https://www.sqlite.org/c3ref/set_authorizer.html) runs at
statement preparation. It cannot enforce current per-row write grants or later
credential revocation by itself. Prepared handles need execution-time checks.

The [attachment limit](https://www.sqlite.org/limits.html) defaults to ten and
has a hard maximum of 125. [WAL](https://www.sqlite.org/wal.html) does not provide
crash-atomic transactions across attached databases. Retaining WAL and co-locating
one atomic group is the recommended starting design, not a claim of federation.

## Small experiment

An in-memory Python SQLite connection attached another in-memory database, created
a physical `d_opaque_items` table and exposed it as a TEMP `items` view. SELECT
returned the expected row. `table_info(items)` omitted the underlying primary-key
and NOT NULL flags, `index_list(items)` was empty, and the main catalog had no
`items` table. Creating an AFTER trigger on that view failed. A trigger on an FTS5
virtual table also failed. This establishes that those shortcuts are insufficient;
it does not qualify Room, native packaging or performance.

## Further primary references

- [SQLite isolation](https://www.sqlite.org/isolation.html): snapshot and writer behavior.
- [Trigger semantics](https://www.sqlite.org/lang_createtrigger.html): generated changes and connection-local triggers.
- [Conflict algorithms](https://www.sqlite.org/lang_conflict.html): statement failure behavior and replacement.
- [Foreign keys](https://www.sqlite.org/foreignkeys.html): cascades and deferred validation.
- [RETURNING](https://www.sqlite.org/lang_returning.html): result timing; returned data is not commit evidence.
- [Room invalidation API](https://developer.android.com/reference/androidx/room/InvalidationTracker): refresh is separate from recording invalidated tables.
- [Room migrations](https://developer.android.com/training/data-storage/room/migrating-db-versions): exported schemas and migration validation.

The following briefs turn these findings into implementation gates. Cross-file
atomicity, secure bundle visibility, primary-key reincarnation and strong SQL
mapping remain design decisions, not demonstrated solutions.
