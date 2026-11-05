# Phase 4 identity-registry evidence

Status: implementation complete (2026-11-06).

The issuer registry is keyed by identity. Records hold admission, profile,
capability, partition grants, labels, claims, trusted grants and revocation
tombstones; linked nodes are supplied by the membership view. Issuer APIs now
list `identities` and label identities, and issuer revocation accepts only an
identity. Partition grants are resolved from the identity link, so a later
node linked by that identity receives the existing grant and group keys.

The migration also updates `DeviceKeys` to `NodeKeys { identity, node }`,
removes issuer node-targeted revoke callers, and keeps the registry outside
application payloads. Self-reported node names are deferred: `ContactRecord`
does not currently carry a name, and the phase 5 discovery format must define
its authentication and size bounds before the registry can expose it.

Verification evidence:

- `cargo test -p rocky-sdk --test group_secrets`: passed (11 tests), including
  grants reaching a later linked node and identity-keyed registry listing;
- `cargo test -p rocky-sdk --lib node::delivery_tests`: passed (16 tests);
- `cargo check --workspace --tests`: passed;
- `cargo check --manifest-path benchmarks/chaos/Cargo.toml --tests`: passed;
- `scripts/commitlint --jj`, `./scripts/lint`, and `./scripts/check` remain
  required workspace gates. Commitlint passed. Rust formatting and clippy
  checks pass; the local lint wrapper cannot run shellcheck/actionlint, and
  the full check wrapper stops because this host's Python lacks `tomllib`.

The legacy registry-revocation fixtures still need a trusted-many-nodes
follow-up: one assumes a later issuer remains usable after the root revokes
its origin (identity revocation intentionally cascades), and another still
expects a node credential while exercising an identity-only record. The
focused identity, group-secret and delivery suites pass.
