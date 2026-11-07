# Phase 2 identity-key evidence

Status: SDK lifecycle implementation complete (2026-11-07). Focused checks and
the workspace unit tier pass. The complete workspace/browser gate is not green.

## Prerequisite integration

The supplied ancestry claim was incorrect. `main` at `7dbaffbb` descended from
`63b3f3cc` and the three Phase 3 credentials commits; it did not include the
other feature lanes. With explicit authorization, the missing changes through
`af94cefba9f6` were three-way integrated into the existing working change:
Phase 1 (`bfe3d7bd143b` plus fixes), Phase 2a (`8800d3764c5c`), Phase 4
(`cec2ad1d7839`) and Phase 5a (`af94cefba9f6`). The browser-backup constructor
ancestor was included; unrelated features_13 qualification prose was excluded.

Phase 3's extension v10, membership sequence/predecessor, deterministic fork
view and 32-node bound remain. The six pre-existing checkpoint/escrow and
features_13 edits were verified byte-for-byte unchanged. No bookmarks, pushes,
history rewrites, extra checkout or extra build-cache directory were created.
Checks reused the configured target directory.

## Implemented lifecycle

- `StoredIdentity`, the identity vault role, `stored_identity` and the retained
  settings provider are removed. The database drops its startup signer; later
  identity signatures use only the call-scoped provider. Routine grants,
  partition/claim delegation, renewal and tip statements use certified node keys.
- Fresh owners use `RootKey::identity()` (HKDF-SHA256, `rocky identity v1`). One
  root phrase restores both keys. The provider port returns an opaque owner
  signer; application root signers otherwise require an explicit owner provider.
- New ordinary joins and default recovery identities are single-use. Their
  generated key certifies the initial node and is not persisted. Local identity
  signing and identity-enrollment answers refuse further links for this mode.
- `Configuration::join_linkable` returns its generated phrase once through
  `Opened.identity`. Supplied providers, single-use joins and reopening return
  none. Phrase and passkey-secret providers work after restart.
- Approval joins retain a transient signer in `AwaitingAdmission`.
  `Configuration::resume_join` validates the installation and invitation, keeps
  the original identity mode, and carries a generated phrase across repeated
  pending responses. No continuation secret is serialized.
- External two-step enrollment uses `Database::node_keys_of` with an explicit
  provider, supplied again at install. The no-provider convenience was removed.
- Settings v30 rejects other settings versions. Identity-enrollment bundles v4
  carry the contacted node's signed certificate as predecessor. SDK author
  numbers follow its membership sequence within the reserved node range;
  providers need no mutable author counter.

The integration also reconciles identity-signed unlinks with node attestations:
the target certificate proves ownership and the unlink remains live for its
permanent certificate. Portable author bindings check table authority on the
identity links, rather than demanding grants on the grant-free node leaf.
Terminal acknowledgments use the direct trusted grant plus the existing node
certificate. Backup rebind removes the predecessor's private line binding.
Checkpoint byte fixtures were regenerated for the current membership encoding.

## Verification

- `./scripts/lint`: exit 0. Its inherited actionlint diagnostic remains at
  `.github/workflows/chaos.yml:86` (arithmetic in an expression).
- `cargo clippy --workspace --all-targets --locked -- -D warnings`: passed.
- `./scripts/check --unit`: 259 passed.
- `cargo check -p rocky-sdk --all-targets --no-default-features --locked`: passed.
- `cargo test -p rocky-credentials --no-fail-fast`: 97 passed, including the
  compile-fail doctest, permanent identity-owned unlink, node-signed delegation
  and portable author-binding regressions.
- Focused SDK command: `cargo test -p rocky-sdk --lib --test identities
  --test credentials --test registry --test author_ranges --test application
  --test grants --test group_secrets --no-fail-fast`: 250 passed, 22 ignored.
  Identity tests scan settings and vault files after create, join, link and
  renewal, restore owner/signers after restart, test both admission modes and
  repeated approval continuations, and check provider release.
- The targeted peers format-rejection test passed after updating its negative
  matrix to reject v29 and v31 around current v30.
- `./scripts/check` was run, including wasm32: the browser run passed 48 of 69
  runs and failed the remainder plus browser-sync. That run preceded several
  fixture/terminal-proof corrections; no passing complete browser gate is claimed.
- Native recheck `./scripts/check --unit --integration`: 259 unit and 1,014
  integration tests passed; the integration gate failed. Remaining cases include
  old certificate fixtures (`tests/scaling.rs:530`, coordinator test support),
  old chain indexing (`tests.rs:874`), branch/migration consumers needing explicit
  identity input or a linkable identity, and unresolved revocation/coordinated
  and desktop cases. The peers format case was fixed and retested afterward.
  Exact named failures from that run are available locally in
  `/tmp/rocky-phase2b-failures.txt`; the complete log is
  `/tmp/rocky-phase2b-native-verified.log`.

These checks do not establish a complete distributed protocol or desktop app.

## Next lane and limitations

- Finish Phase 3 runtime membership propagation/fork enforcement and replicated
  identity-owned unlink with reader-key rotation. The contacted certificate is
  not yet a globally current replicated membership head. The storage layer's
  separate line-node helper and issuer-carried node-target statements also need
  reconciliation with membership authority.
- Additional per-line writer nodes need an explicit identity call. Single-use
  identities cannot create them. Branch/migration consumers must choose a
  linkable identity and supply its signer; the remaining suite has old callers.
- An unfinished generated join cannot recover its key after process death or a
  lost response without a retained continuation. Use an application signer for
  restart-safe enrollment, or a fresh invitation. Providers and continuations
  are caller-owned in memory; the database retains neither as a default signer.
- Phase 5b seen-node discovery/code-confirmed linking and Phase 6 trusted identity
  behavior remain outside this lane. Update consumers and remaining certificate
  fixtures, then repeat the full native/browser gate.
