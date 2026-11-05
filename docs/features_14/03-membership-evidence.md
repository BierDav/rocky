# Phase 3 membership evidence

Status: implemented in the credentials layer.

- Node certificates carry a membership sequence and predecessor in extension
  format 10. `CredentialChain::certify_member` requires sequence zero to have
  no predecessor and later links to name one.
- `MembershipSet` retains all certificates for an identity and reports a
  deterministic fork when two certificates occupy one sequence.
- Issuer revocations naming nodes are refused. An identity-signed revocation
  may name its own linked nodes; root identity revocation remains unchanged.

Checks: `cargo test -p rocky-credentials --test credentials --test identities
--test root --test tips`; focused membership unit test passed.
