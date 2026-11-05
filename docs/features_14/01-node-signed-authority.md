# 1. Node-signed authority

Status: planned (2026-11-05).

The identity key leaves the hot path. Everything a node signs routinely, it
signs with its own key under its identity's authority. This is the
prerequisite for identity keys held as a phrase or passkey.

## Today

- `node/tips.rs` `Signer` endorses seals, tip openings, bookmark moves,
  genesis lines and checkpoint records with `self.identity`.
- `Settings::authority` builds `Authority::issuer(identity, links)`. Every
  capability, revocation and renewal answer an issuer signs needs the
  identity provider.
- `CredentialChain::certify` copies the identity link (`..last.clone()`), so
  a node certificate expires with the capability and is re-signed at every
  renewal.
- `IdentityRequest` and `RenewRequest` differ: the first is signed by the
  identity too, the second by the node only.

## Changes

- **Certificate validity.** A node certificate (`PrincipalKind::Node`)
  carries `expires = NEVER` and no grants. `links` verification stops
  requiring a node leaf to narrow its parent's validity. It requires
  instead that the parent capability is valid at the time and the node is
  not unlinked. Grants are read from the identity link.
- **Separate capability and certificate.** `Credentials` holds the
  identity's capability chain and the node certificate as two parts. A
  renewal replaces the capability chain only.
- **Endorsement by node.** `Endorsement` gains the node certificate. The
  signature is the node key's; `signer()` still returns the identity, and
  the lane is the node. `TipGraph` admission, `Cut` and equivocation
  checks keep their `(identity, lane)` keys.
- **Issuer authority on the node.** `Authority` takes the node key and the
  node's credentials instead of an `IdentityProvider`. Verifiers accept an
  issuer statement whose signer is a node certified by an identity whose
  capability issues.
- **Renewal answers.** Issuer answers in `enroll.rs` are node-signed under
  the issuer's credentials.

## Done when

- A trusted node seals, opens tips, moves bookmarks, admits, renews,
  grants and revokes for a day of simulated time with its identity
  provider removed after installation.
- A revoked trusted identity's node-signed statements past the root's cut
  are refused (`TipStatementsPastCut`), with lanes per node.
- `rocky-credentials` tests cover a certificate outliving three capability
  renewals, and a certificate refused once its identity is revoked.
