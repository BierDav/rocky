# features_17: schema grant families

Status: implemented and validated (2026-11-10).

## Authority and compatibility

The SDK signs one resource grant and one partition selector per `granted_like`
family, retaining explicit follower grants whenever their complete authority differs.
Partition-group membership alone grants nothing. Whole-row, whole-field grants
inherit; ciphertext-only custody also inherits its empty field set. Restricted rows,
columns, rights and selector values stay paired. An explicit follower selection
suppresses inheritance for that follower. Compaction compares both sides, including
when grants contain duplicates.

The capability extension has an optional, signed `families: true` field. Its absence
means exact per-table authority, including on sparse old credentials. The field is
omitted when false: existing extension bytes, signatures, IDs, revocations and stored
credentials retain their meaning. Extension version 11, capability signing version 3,
SDK settings version 32 and the replica file format are unchanged. Existing per-table
credentials need no migration. Owner holds and ordinary renewals compact when they
are signed again. Delegations are reissued by their delegator as before.

Portable author bindings keep their format and signature structure. A marked
whole-table assignment uses a domain-separated table commitment
`commitment("rocky.author-binding.grant-family.v1", table_id)`; old assignments keep
the plain table ID. Verifiers resolve qualified assignments against the authenticated
source schema of the creation proof. An old assignment never gains follower authority.

Every marked link requires an authenticated schema. Delegation checks both the
parent's and child's resolved authority, preventing a child from dropping an explicit
follower restriction. Node certificates, issuer/trusted authority and claims do not
carry family grants. Revocation, author ranges, membership sequence, validity, replay
and partition tuple/name checks remain in force. Cached verification includes the
family context; cached capabilities stay compact and share the compiled family map.
Ordinary author checks resolve only the requested table.

Old runtimes reject the unknown signed extension field. Upgrade every application
replica before relying on family credentials; downgrade or mixed-runtime operation
with new credentials is unsupported. Already-issued exact credentials remain usable
by the new runtime without re-signing.

## Limits and remaining costs

There is no fixed table-count quota. The existing definition budget is 1 MiB, logical
schema and manifest budgets are 4 MiB, and each manifest item group is limited to
32,768 entries. The SDK still permits at most 128 declared columns per table. These
bounds cover definition decoding, canonicalization, generated SQL and metadata;
SQLite projections, indexes, keys and retained operations still cost space per table.

A signed capability still holds at most 64 resource and partition grants combined;
its extension is at most 64 KiB. A credential structure is at most 256 KiB and a chain
has at most eight links. For uniform partitioned families an owner uses `2F + 1`
grants, including the issuer registry; a writer or reader uses `2F`. Explicit
exceptions and independent families consume the remaining grant budget. The
registry's independent limit of 256 recorded partition grants per identity remains.

Sealed delivery wrappers now use the credentials layer's existing 112 KiB payload
budget, replacing the SDK's 37 KiB bound. The inner group envelope remains limited
to 4 KiB, and secure frames remain limited to 256 KiB. An unusually deep or complex
credential chain can still exceed the delivery budget and is rejected.

Frontier messages remain bounded to 64 scopes and the existing encoded-frame budget;
wide partition groups span multiple pages. Local SQL sessions are bounded by their
installed, verified authority rather than a separate 256-scope count. Registered
concrete partition scopes still have a 4,096-entry bound per replica, so a 100-table
group can occupy 100 entries for each distinct partition value. Group secrets are
shared and rotated once per group partition; member keys are still derived and
installed per table. No capacity or throughput benchmark is claimed.

## Correctness evidence

The native SDK regressions create 31- and 100-table schemas in one family, install
owner/writer/reader credentials, share the partition secret, replicate a write to a
follower, refuse a reader's update, revoke the writer and deliver the rotated epoch
to the remaining reader. A 31-table exact credential reopens, shares, and compacts
on rotation. A maximum-size inner envelope with this exact credential exercises a
wrapper above the former SDK bound. A separate 160-table database writes and reads
without networking through a session covering more than 256 authorized scopes.

Credential tests cover exact and marked credentials, sparse old authority, portable
assignment proofs, selector remapping, row/column restrictions, custody, duplicates,
expiry, author spoofing, marker tampering, grant quotas and delegation attenuation.
Renewals preserve the signed exact/family choice. Schema-upgrade and shared-list
regressions also cover existing members, old invitations and saved branch writers
adopting family authority. Exact-chain caches remain independent of schema context;
marked ancestors still bind their cache entry to the authenticated family map.

Validation commands (2026-11-10):

- `./scripts/lint`: passed.
- `./scripts/check --unit`: 288 passed.
- `cargo test -p rocky-credentials`: 111 ordinary tests and one doc test passed.
- `cargo test -p rocky-sdk --lib node::wide_schema_tests -- --nocapture`: four passed.
- `cargo test -p rocky-sdk --test generations --test shared_lists`: 33 passed.
- `./scripts/check`: passed native workspace Clippy, SDK compilation without
  networking, 288 unit and 1,194 integration tests, wasm32 Clippy, all 933
  headless-browser tests (73 runs), and browser/native relay sync.

The full matrix passed before rebasing onto the independent coordinator-capacity
commit `492f177a`. No grant-family source changed in the rebase. Additional
checks on the combined tree passed:

- `cargo test -p rocky-coordinator --locked`: 74 tests, including its new capacity
  and large-snapshot cases.
- `cargo clippy -p rocky-coordinator --all-targets --locked -- -D warnings`.
- `CLIPPY_CONF_DIR="$PWD/scripts/clippy-wasm" cargo clippy --locked --target wasm32-unknown-unknown --tests -p rocky-coordinator -- -D warnings`.

The separate native e2e tier (`./scripts/check --all`) and throughput benchmarks
are not part of this validation.

Measured canonical CBOR sizes from a focused native run (bytes; random keys cause
small variations):

| Schema and credential | Grants in busiest link | Owner chain | Writer chain | Reader chain | Share payload | Sealed delivery |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| 31 tables, family | 3 owner / 2 member | 6,506 | 6,423 | 6,360 | 7,190 | 7,354 |
| 100 tables, family | 3 owner / 2 member | 6,565 | 6,489 | 6,426 | 7,246 | 7,413 |
| 31 tables, existing exact owner | 63 owner / 2 new member | 37,103 | 6,419 | 6,356 | 37,792 | 37,958 |

These are complete sender chains and actual share wrappers, rather than just the
individual table grants. The exact owner compacted to a 6,503-byte chain on rotation.

## Paperless adoption

Release the coordinated Rocky crates with these changes. Update every exact Rocky
pin in Paperless to that release: the workspace SDK, `paperless-node`'s SDK/core/
coordinator, `paperless-core`'s platform and the banking vault's credentials crate.
Regenerate its Cargo lockfile. Remove the `rocky-sdk` entry under `[patch.crates-io]`
and the vendored SDK; the delivery patch is included upstream. No application API
or schema-format change is required. Keep the current partition group and
`granted_like` declarations, and declare new tables with the same family anchor.
Use the normal schema-generation publication flow for existing databases; do not
reset them or edit stored credentials. Explicitly renew/reissue credentials if
compact authority is needed before the next ordinary renewal or schema update.

Low-level consumers can use `GrantFamilies::new` with their authenticated manifest
and canonical table scopes, the family issuance/delegation methods and
`verified_with_families`. `SecureConfig::with_table_scopes` supplies this context at
replica startup; existing per-table opening calls remain supported. Full literals of
`Extended` need its new `families` field, or `..Extended::default()`. Paperless does
not construct that low-level type.
