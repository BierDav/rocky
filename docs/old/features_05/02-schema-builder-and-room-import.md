# 2. Schema builder and Room import

Status: slices 1–3 implemented and measured; required builder/import result met. Slice 4 waits for
phase 7's gate. Owner: `sdks/kotlin/rocky-core` schema API and
`rocky-core` definition data. Depends on nothing to start; takes the implicit
columns of [phase 7](07-row-identity.md) as a late slice. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 2](agreed-scope.md#2-schema-declaration) of the scope,
[schema definitions](../runtime/schema-definitions.md),
[logical schemas](../runtime/logical-schema.md) and the Kotlin `Schema`
builder. Inspect what definition data the core already accepts: compound
primary keys and real `PRIMARY KEY` clauses exist, the Kotlin side cannot
declare them.

Make the Kotlin builder a thin builder for definition data and make Room's
exported schema fill the same builder, so a table is declared once and both
paths produce one descriptor. Later phases add attributes to this builder:
partition columns ([phase 11](11-partitioned-scopes.md)), unique indexes,
foreign keys, conflict policies and delete actions
([phase 12](12-hidden-rows-unique-indexes-and-foreign-keys.md)). Design the
builder so those are additive.

## Ordered slices

1. Compound primary keys, non-unique indexes and `real` and `blob` columns in
   the Kotlin builder, emitted as definition data; remove the generated
   `id TEXT PRIMARY KEY` default once the implicit columns of slice 4 exist.
2. Room import: fill the builder from Room's exported schema JSON for the
   pinned Room version. Rocky-only attributes are added on top in Kotlin.
   This replaces the separate policy JSON of logical schemas.
3. One descriptor: nothing downstream distinguishes a hand-built schema from
   an imported one; fingerprints are equal for equal declarations.
4. After phase 7's gate: `id` and `row_kind` exist on every table and are not
   declared; refuse an entity that declares them with another type and refuse
   `autoGenerate = true` keys at import with a clear error.

## Completion gate

- A schema with a compound key and a non-unique index installs on the trusted
  and the secure profile and the typed table shows the real clauses.
- The shopping and market schemas import from Room's export and produce the
  same descriptor as the hand-written builder.
- `real` and `blob` round-trip through capture, bundles and Room.
- Unsupported Room features fail at import with the offending entity named.
- [Schema definitions](../runtime/schema-definitions.md) and
  [Room integration](../runtime/room-integration.md) describe the single path.

## Measured evidence

Slice 1 (2026-10-07): `./sdks/kotlin/check-schema` compiled with `-Werror`
and passed builder/key/type/index and refusal checks. `cargo test -p rocky-sdk
--lib --test queries --locked` passed 5 SDK unit tests and 6 trusted query tests
(1 measurement ignored). `cargo test -p rocky-secure definition_compound --locked`
passed encrypted compound-key capture, REAL/BLOB bundle application and reopen.
Both profiles enforce the compound key and reopen with the declared index.
This does not yet qualify a Room DAO or the later implicit columns.

Slice 2 (2026-10-07): `./sdks/kotlin/check-schema` passed import equality for
shopping, market-shaped and scalar schemas and entity-named refusals for
auto-generation, FTS, CHECK, trailing SQL, foreign keys, unique indexes and
metadata/SQL disagreement. `./kotlin test -m schema-qualification` regenerated
Room 2.8.4 exports and passed its REAL/BLOB/null and compound-key DAO test.
The market export is a new qualification fixture matching its version-2 shape;
the market application has no existing Room export. Room still uses its own file.

Slice 3 (2026-10-07): `./sdks/kotlin/check-schema` passed canonical descriptor,
full fingerprint and replicated fingerprint equality for shopping, market-shaped
and scalar hand/import pairs. All three Kotlin schema inputs installed through
`Database` and reopened using the imported input. Direct SQLite inspection
confirmed the compound key and descending index. Reordered tables and Boolean
INTEGER defaults normalize to identical definition data. Invalid legacy
attribute declarations, partial indexes, expression defaults and views are
refused explicitly.

`./kotlin test -m schema-qualification` passed two tests: Room scalar/key/null
behavior and Kotlin builder installation/reopen through JVM UniFFI using the
Room-imported schema. REAL/BLOB capture and encrypted bundles are qualified
separately in the Rust tests; this is not a Rocky Room driver qualification.

### Prerequisites and remaining boundary

Core definition format 1, `TypedApp`, `TypedRows` and profile diagnostics already
provide compound keys, non-unique indexes and strict scalar storage. The missing
SDK/native schema-input adapter was repaired in slice 1; admission still runs
before installation. No descriptor or replication format was changed.

The required result (one Kotlin builder for definition data, filled by hand or
from Room's export) is met. The late slice 4 remains incomplete: phase 7 must
first supply implicit INTEGER `id`/`row_kind` and its secure incarnation gate.
The current workspace still has the legacy generated TEXT key and key-based
secure identity. The new builder currently declares keys/columns explicitly;
it neither synthesizes the later columns nor claims their entity checks.
`autoGenerate = true` is already refused at import. Partitions and constraint
policies remain their owning phases. The legacy register and Rust Room/policy
paths preserve existing descriptor identities rather than migrating them silently.

### Final verification

On 2026-10-07, the final `./scripts/check` exited 0: formatting, workspace
Clippy with warnings denied, workspace tests/doc tests and `rocky-sdk` tests
without default features passed. `./sdks/native/check` passed both default and
no-default ABI builds; `./sdks/kotlin/check` passed the existing JVM/process
contract. The final schema/import and Room/JVM qualification commands above
also passed. An earlier workspace run failed a new catalog assertion that
counted both live and checkpoint indexes; the assertion was scoped to the live
table and the full check rerun passed. Measurement/soak tests marked ignored
remain outside this evidence. Logs are in the workspace's ignored
`local/phase2/` directory.
