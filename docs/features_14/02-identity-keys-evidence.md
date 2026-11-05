# Phase 2a identity-key evidence

Status: implementation complete; the single-node identity port is rebased on
the Phase 1 node-signed authority lane (2026-11-06).

The lane adds `IdentityKey` with the same 24-word phrase and 32-byte secret
encodings as `RootKey`, derives the owner's identity with HKDF-SHA256 labelled
`rocky identity v1`, and keeps root and identity signing types distinct. A
session can carry an identity provider for one call with
`Session::call_with_identity`; requests can instead carry an identity phrase
or secret. The provider is checked against the identity recorded by the
database before it signs. The call-scoped provider is held in a task-local and
dropped when the call completes.

Verification evidence:

- `cargo test -p rocky-credentials`: passed (93 tests, including the
  `IdentityKey` compile-fail doctest);
- `cargo test -p rocky-sdk --test identities`: passed (3 tests);
- `cargo check --workspace --tests`: passed;
- `cargo test -p rocky-sdk --lib`: passed (197 tests, 22 ignored);
- `./scripts/lint`: passed; actionlint still reports the pre-existing
  expression parse at `.github/workflows/chaos.yml:86`;
- `scripts/commitlint --jj`: passed;
- `./scripts/check --unit`: 237 passed, 17 failed in existing coordinator
  tests that still construct legacy authority fixtures.

The full `./scripts/check` gate is not repeated here because the Phase 1
evidence records the corresponding coordinator and rocky-secure legacy
fixture failures. This phase changes only credentials and SDK call plumbing;
no additional full-suite limitation was observed in the focused checks above.
