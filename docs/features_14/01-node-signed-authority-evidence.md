# Phase 1 node-signed authority evidence

Status: implementation complete; workspace gate has remaining legacy fixture failures (2026-11-06).

The lane ports routine issuer signing to the node key and keeps the identity
key only for node certification. Node certificates are non-expiring, carry no
grants, and retain the identity capability chain separately. Issuer authority,
revocations, discovery values, claims, tip statements, renewals, and terminal
credentials use the node signer. Grant and partition consumers read the held
identity link instead of the certificate leaf. Trusted health collector proof
normalizes a full link chain to the two-link trusted-grant plus certificate
form required by `trusted_node`.

Format changes:

- replica file identity: v57 (`crates/rocky-secure/src/replica/generation.rs`);
- network handshake: v25 (`crates/rocky-secure/src/network/link.rs`);
- SDK settings: v28 (`crates/rocky-sdk/src/enrollment.rs`);
- frozen checkpoint vectors regenerated: `backend_vectors/checkpoint-id.bin`,
  `signature.bin`, `signed.cbor`, and `signing.bin`.

Verification evidence:

- `cargo check --workspace --tests`: passed;
- `cargo test -p rocky-credentials`: passed, 89 tests;
- `cargo test -p rocky-sdk --lib`: passed after the final grant and health
  fixes (the pre-fix run was 193 passed, 4 failed, 22 ignored; the four
  health tests then passed in `cargo test -p rocky-sdk simulation::health --lib`);
- `./scripts/lint --fix` and `./scripts/lint`: passed the repository checks;
  actionlint still reports the pre-existing expression parse at
  `.github/workflows/chaos.yml:86`;
- `python3 crates/rocky-secure/src/replica/checkpoint/backend_vectors/verify.py`:
  passed and checked 28 frozen files;
- `./scripts/check --unit --integration`: ran to completion but currently
  fails in coordinator and rocky-secure tests that still construct legacy
  self-signed or grant-bearing node certificates. The unit tier reported 236
  passed; integration reported 625 passed before dependent failures.

The remaining gate failures are test-fixture migration work in
`rocky-coordinator` and `rocky-secure`; production SDK and credentials tests
exercise the Phase 1 authority path successfully. No checkpoint or compaction
source files were changed beyond the regenerated checkpoint vectors.
