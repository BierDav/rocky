# Trusted identities with many nodes evidence

Status: implemented and verified; required workspace gate exits 0 (2026-11-07).

## Behavior

Trust remains a root grant to a database-local identity. Its linked nodes use
that grant through their own certificates; adding siblings uses Phase 5b's
two-sided code confirmation and a call-scoped identity signer, without a root
ceremony. No identity secret is added to database state.

Trusted observers persist certified links and identity-owned unlinks, including
those of their own identity. Repeated sync/status and reopening expose the same
`TrustedNodeLinked { identity, node, linked }` and
`TrustedNodeUnlinked { identity, node, unlinked }` observations without creating
duplicates. Link time is the observer's first-seen time, rather than the
certificate's inherited capability start. Registry trust selection verifies the
embedded root grant, its identity and database context before observation or
trust-list use. Foreign or misnamed root grants are refused; a valid future
grant is not active early.
Discovery hints do not create trusted observations. Historical
observations remain visible after root revocation.

`TrustedIdentityStale` replaces `TrustedNodeStale`. Last-heard evidence is saved
per node; an identity's most recent currently linked node determines staleness.
A live sibling keeps the identity heard, while evidence from an unlinked node
cannot extend it. Health remains informational per node. Health wire version 3
and signing label `rocky.sdk.health-report.v2` bind reports to `DatabaseContext`
(pinned publisher root and stable mesh/application/domain). Foreign signatures
fail before health or staleness updates. SDK settings version 32 stores the
local observations and per-node heard times; only that version is accepted.

Root revocation cuts every currently linked lane, including lanes with no tip
statements, within the existing 32-node bound. Identity-owned unlink carries
that node's lane cut and retains it permanently. Historical unlinked lanes do
not consume the subsequent root cut's 32 slots. An unlink cannot cut a different
identity or a node it did not unlink. Root heads supersede unlink heads for the
same lane in either arrival order, so a later unlink cannot widen a root cut.
Repeated unlinks keep the first signed unlink's heads under either merge order.

The coordinator's trusted list continues to name identities. It checks the
certificate's context and signature, current root revocations, identity-owned
unlinks and membership forks before authorizing trusted operations. Unlinks
arrive through the existing verified authority/revocation exchange; surviving
siblings continue to work.

## Regressions and checks

The SDK regression alternates seals between two nodes of one trusted identity,
links a third without the root and checks exactly one link alert on all four
trusted observers (owner and all three siblings). It unlinks that third node,
checks all surviving observers and denied sync, revokes the identity with both
remaining lanes, and verifies their earlier seals and durable observations at
reopen. The two-database flow covers separate roots and a shared root with
different stable scopes, with the identity trusted in A and ordinary or revoked
in B. Foreign link/invitation evidence leaves settings, pending requests and
credential installation unchanged; A's unlink and root revocation leave B's
keys, registry, trust observations and access outcome unchanged.

Deterministic credential coverage keeps statements on both lanes through an
unlink and a root cut, refuses later statements, refuses foreign lane cuts and
checks root precedence under both merge orders. Deterministic staleness checks
alternate heard nodes, remove one and restore saved per-node evidence. A health
signature regression covers foreign roots and foreign stable scopes. A
coordinator regression admits three certified siblings of one listed identity,
learns an unlink and refuses that node's subsequent claim while siblings claim.

Final measured checks:

- Required `RUSTC_WRAPPER= ./scripts/check`: exit 0;
  `/tmp/rocky-phase6-full-check.log`. Workspace Clippy and the SDK all-target
  compile without default features pass. Native unit tier: 264 passed (2.48s);
  integration tier: 1,178 passed (86.08s), no failures. The no-default SDK was
  compiled, not runtime-qualified.
- Wasm Clippy passes. All 71 browser runs pass, with 908 selected tests in 68
  binaries (186s). The browser/native relay probe passes bidirectional writes
  and coordinated claims; its single browser test also passes. These results
  are in the same full-check log. Timing is diagnostic.
- `./scripts/lint`: exit 0; `/tmp/rocky-phase6-lint-final.log`. The inherited
  `.github/workflows/chaos.yml:86` expression diagnostic remains; the wrapper
  continues through its regression checks and returns success.
- `./scripts/commitlint --jj`: exit 0;
  `/tmp/rocky-phase6-commitlint-final.log`.
- All six protected files match their starting SHA-256 hashes;
  `/tmp/rocky-phase6-protected-final.log`.

Earlier development checks:

- Development `RUSTC_WRAPPER= ./scripts/check --unit`: 263 passed before
  the final registry/health extension; `/tmp/rocky-phase6-unit.log`.
- Coordinator sibling/unlink regression: 1 passed;
  `/tmp/rocky-phase6-coordinator.log`.
- SDK identity suite before the final isolation/health extension: 13 passed;
  `/tmp/rocky-phase6-identities.log`.
- Credential lane-cut regression: 1 passed;
  `/tmp/rocky-phase6-cuts.log`.

The initial full gate is retained at
`/tmp/rocky-phase6-full-check-initial.log`: 263 native unit passes and 1,119
integration passes, with 59 integration failures and no missing cases. The new
registry guard incorrectly used the node-chain verifier for a root-only grant;
it now checks the grant context and root signature directly. That browser run
passed 70 of 71 runs, with the new future-grant test failing because its fixture
omitted the issuance clock skew. The corrected fixture uses the signed start.
Browser/native relay writes and claims passed in that snapshot. Focused native
health and clock runs then exposed four expectations that omitted the required
persistent link alerts; those expectations now include the alerts. These
intermediate failures are not passing final results.

The next full snapshot (`/tmp/rocky-phase6-full-check-version-fixture.log`)
passed 264 native unit tests and 1,177 integration tests. Its sole native
failure was the version-refusal fixture still treating current settings version
32 as unsupported. That fixture now pins 32 and refuses every earlier version,
the next version and the maximum sentinel without writes. This snapshot passed
all 71 browser runs (908 selected tests in 68 binaries), wasm Clippy and the
browser/native relay write-and-claims probe. The final passing gate above
includes the corrected version fixture.

## Scope and preservation

`cargo metadata --no-deps --format-version 1` establishes credentials as the
membership/cut owner, coordinator as the generic authorization owner and SDK
as the observation/lifecycle owner. Existing Phase 3 and Phase 5 evidence and
implementation remain prerequisites. In-repo alert consumers and the root
README use the current terminology.

All six protected files are audited against
`/tmp/rocky-phase6-protected-start.sha256`. This includes the two explicitly
approved transfer additions inherited from Phase 3; no further transfer or
checkpoint exception is used. Work stays in the shared current jj change,
without commits, bookmarks, push, history rewrites, worktrees or new build/cache
directories. The current change remains `lzmuuprq`, its parent is `qouwotvn`
(`0d031c15`, `feat(credentials): finish with phase 3`), and main remains
`7dbaffbb`. Final jj status is saved at `/tmp/rocky-phase6-jj-final.log`.
Existing Phase 5b edits and the external features_15 work are preserved;
features_15 is not implemented or edited here.

Paperless remains unavailable external consumer integration. Applications own
passkey ceremonies, QR/deep links, database selection and personal directory
bootstrap/synchronization/recovery. Rocky adds no SDK accounts or directory.
The required workspace/native/browser gate does not establish external
consumer, desktop, general distributed, full e2e or soak qualification. Ignored
Docker cases are not claimed as passes. Completion of the in-repo phases does
not mean all consumer work in features_14 is finished.
