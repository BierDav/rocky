# Phase 3 membership evidence

Status: implementation complete and verified (2026-11-07).

## Database isolation implemented (2026-11-07)

`DatabaseContext` binds the pinned publisher root to the existing stable
`Scope` (mesh, application and domain). It excludes schema generations,
bookmarks and paths. Certificates and signed unlinks carry this context;
chain verification checks it against grants, partitions, claims and trust.
Membership views and authority sets hold one context, so sequence heads,
author numbers, forks and active-node bounds are independent per database.

Expected-context checks precede credential installation and authority learning
in secure ingress, SDK enrollment, link acknowledgments, renewal, authority
imports and membership views. Admission authenticates the context before
consuming invitation attempts. Contacts and invitations pin it; identity
requests sign it and encrypted answers must match it. Foreign evidence signed
by the same personal identity cannot mutate local membership or revocations.
The existing explicit identity-provider boundary remains; applications own
passkeys, QR, database selection and encrypted directory bootstrap/recovery.

Current formats: extension 11, authority set 3, revocation signing label
`rocky.revocation.v7`, handshake 27, replica identity 58, SDK settings 31,
invitation 7, contact 3, ordinary enrollment answer 5, identity request 2,
identity bundle 6 and saved line nodes 2. Only these current formats are used.
The authority-set byte bound is 4 MiB: scoped proofs at the required 1,024
live-statement bound exceed the old 2 MiB limit. The signature-size accounting
bound is 1,024 bytes (the scoped fixture measured approximately 989).

Credential regressions exercise distinct roots and a shared root with distinct
mesh/domain, independent sequence-zero histories and 32-node bounds, and
atomic foreign certificate/unlink/merge rejection. SDK regressions exercise
foreign authority bytes, identity requests and replies, invitation-context
tampering, independent grants, unlink/rotation and identity revocation.
Registry and admission use their existing implementations; no phase restart.
Coordinator foreign-certificate refusal is a prerequisite isolation check;
Phase 6 trust alerts, staleness and complete lane cuts remain separate.

## Implementation

- The replicated authority set carries permanent identity-signed certificates
  alongside revocations, in authority-set format 3. Membership evidence joins
  by union, survives revocation pruning, contributes to the exchange digest,
  and travels in bounded pages during the revocation exchange. Statement-only
  encodings are refused. Valid links do not change checkpoint revocation
  digests; void links do. Handshake format 27 and existing-link reconciliation
  refresh the set, including while no rows change.
- Membership sequences retain void forks and unlinked history. Both fork
  certificates are refused, existing siblings remain usable, and a recovery link
  uses the next unused sequence with the surviving head. The 32-node bound counts
  active nodes; an unlink frees a slot without recycling its author number.
- Enrollment bundle format 6 carries the replicated set. The joining node sends
  its signed certificate back to the answering node, which saves it before sync.
  Lost acknowledgments reuse the recorded certificate. A void link discards its
  installation intent so retrying uses a fresh node key.
- The runtime membership view uses certificate evidence rather than issuer
  discovery records. Health reports include `MembershipFork` for the local
  identity. Certificate learning alone does not start checkpoint import or end
  otherwise valid live links. A separate membership observation wakes link
  publication without advancing the row-projection revision.
- `Session::unlink(node, identity)` uses a call-scoped identity provider and the
  target certificate as ownership evidence. Issuers learn the permanent unlink
  and rotate exposed collection and partition scopes through the existing
  rotation intent. Node targets are refused in issuer statements; identity
  revocation no longer adds node targets. The last node may unlink itself;
  its identity keeps its permissions. It can publish signed evidence through
  the renewal transport without authenticating as the now-unlinked node.
- Additional line writers use membership sequences, predecessors and active-node
  bounds, with identity signing outside replica transactions.

## Regression coverage

The credentials regressions cover fork convergence, both void certificates,
relinking, permanent identity-owned unlink, and 32 active nodes with author 33
following an unlink. SDK regressions cover a membership fork alert and a
three-sibling unlink with reader rotation and surviving writers. Final gate
results and limits are recorded below.

Five protected predecessor files remain byte-for-byte unchanged against
`/tmp/rocky-phase3-protected-start.sha256`. The user authorized two focused
exceptions in `replica/checkpoint/transfer.rs`: `holds_anything` recognizes
retained replayed-table rows, and checkpoint page accounting initializes
`used` from `encode(&plain)?.len()` to reserve signed metadata and escrow
proofs before filling rows. The second exception was explicitly approved after
reviewing `/tmp/rocky-phase3-proposed-page-budget.patch`. Removing only these
two additions reproduces the saved starting file byte-for-byte
(`/tmp/rocky-phase3-transfer-before-authorized.rs`). No bookmark, checkout,
history rewrite or push was performed. Phases 5b and 6 remain separate lanes.

## Check snapshots

- `cargo test -p rocky-credentials --no-fail-fast --locked`: 99 passed,
  including the compile-fail doctest (`/tmp/rocky-phase3-credentials-verified-final.log`).
- Focused SDK snapshot before the existing-link refresh: 203 library tests
  passed, 22 ignored; the fork alert/relink and last-node credential tests
  passed. All eight identity integration tests and both concurrent revocation
  tests passed (`/tmp/rocky-phase3-focused-final.log`).
- That snapshot has one reproducible failure:
  `simulation::regressions::a_member_on_another_branch_recovers_from_listed_bases`.
  Seed 10961 reaches a projection/full-rebuild mismatch on `items` for newly
  admitted node `d` after a branch and revocation. The isolated rerun reproduces
  it (`/tmp/rocky-phase3-branch.log`); it is not classified as a pre-existing
  passing result. Existing-link refresh and final workspace checks are pending.
- Checkpoint signed fixtures were regenerated for the current certificate and
  authority digest. The normal fixture assertion is part of the final gate.

## Branch recovery and fresh verification

Seed 10961 exposed a retained-prefix recovery bug. Rebootstrap now rebuilds
retained canonical frames after clearing replayed state; checkpoint transfer
recognizes retained replayed rows and stages replacement instead of inserting
duplicate row IDs. A full audit explicitly returns `Blocked(StagedImport)`
while a direct replayed import is partial, then performs the complete comparison
after its atomic switch. It never reports a successful audit of partial state.

- Seed 10961 regression passed: `/tmp/rocky-phase3-branch-authorized-fix.log`.
- Partial-import audit regression passed:
  `/tmp/rocky-phase3-partial-audit-test.log`.
- Coordinator foreign trusted certificate regression passed:
  `/tmp/rocky-phase3-trust-isolation.log`.
- Scoped credentials snapshot: 103 tests including doctest passed
  (`/tmp/rocky-phase3-scoped-credentials-strict2.log`).
- Workspace clippy passed (`/tmp/rocky-phase3-scoped-clippy.log`).
- Earlier lint exited 0, retaining the inherited chaos workflow diagnostic
  (`/tmp/rocky-phase3-scoped-lint-final.log`).

These are measured snapshots, not a complete workspace/browser success claim.
The full gate was restarted after an interrupted compilation; the replacement
uses `RUSTC_WRAPPER=` because the shared sccache process stalled on `rustc -vV`.
It reuses the existing target directory and does not stop unrelated builds.
Final results are recorded below after that gate settles.

Fresh native results after scoped integration:

- `cargo test -p rocky-credentials --no-fail-fast --locked`: 103 passed,
  including doctest (`/tmp/rocky-phase3-credentials-resumed.log`).
- SDK library binary: 206 passed, 22 ignored, including seed 10961,
  fork/relink, last-node unlink and scoped ingress
  (`/tmp/rocky-phase3-sdk-library-resumed.log`).
- Two-database integration passed with distinct and shared roots, independent
  histories/grants, foreign invitation rejection without consuming it,
  reader-key rotation, unlink and identity revocation in A, and unchanged B
  registry/profile and access (`/tmp/rocky-phase3-two-databases-final.log`).
- Three focused SDK foreign-evidence tests passed, including the encrypted
  identity-answer and pre-mutation runtime refusal regressions
  (`/tmp/rocky-phase3-foreign-final.log`).
- Checkpoint vectors regenerated for scoped certificates; the rebuilt binary's
  ordinary frozen assertion passed
  (`/tmp/rocky-phase3-vectors-normal-rebuilt.log`). The independent Python
  verifier checked all 28 frozen files
  (`/tmp/rocky-phase3-vectors-python.log`). An initial ordinary assertion used
  the old binary's embedded bytes and failed; it is superseded by this rebuilt
  assertion, not counted as a pass.
- Final consumer lint exited 0
  (`/tmp/rocky-phase3-lint-consumers-final.log`), retaining the inherited
  `.github/workflows/chaos.yml:86` expression diagnostic. Commitlint passed.

The first resumed full gate's native snapshot reports 248 unit and 1,103
integration passes and fails overall. It includes fixtures corrected during
that run; it is not a result for every final test edit. The exact 69 named
failures emitted by that snapshot are in
`/tmp/rocky-phase3-native-resumed-failures.txt`, with full output in
`/tmp/rocky-phase3-scoped-check-resumed.log`. These counts are reported as
printed, without inferring failed counts from tests omitted after a binary
failure. Focused reruns of the corrected fixtures and browser results follow.

Further consumer verification:

- Coordinator library: 62 passed, including scoped trusted-certificate refusal,
  claims, counters and coordinator persistence
  (`/tmp/rocky-phase3-coordinator-current.log`). Its test certificates now use
  the control's pinned publisher/scope; root claim revocations are scoped.
- SDK generation consumers: 21 passed, including schema changes and returning
  from migrated branches. Custody: 4 passed, 1 Docker case ignored
  (`/tmp/rocky-phase3-consumers-current.log`). These fixtures supply a
  caller-owned identity at reopen and per-call for branch publication.
- That consumer snapshot's migration suite passed 13 with 2 ignored and one
  obsolete identity-signed-descriptor expectation failing. The corrected test
  requires node signing despite an identity provider refusing migration
  signatures, and checks reopening does not repeat its SQL; its rerun belongs
  to the final native gate below.
- Current workspace clippy passed after the consumer corrections
  (`/tmp/rocky-phase3-clippy-current.log`).
- A complete focused secure snapshot passed 130, failed 3, ignored 1
  (`/tmp/rocky-phase3-secure-focused-complete.log`). The failures were stale
  signature-field tampering, expected-set equality omitting retained
  certificates, and a payload-flush injection intercepted by descriptor
  metadata. Those fixtures are corrected; final-gate results below supersede
  this snapshot only where explicitly measured.

## Revoked dependency proofs and membership evidence

The broader native run exposed a runtime distinction, not just stale fixture
expectations: `RevocationSet::is_empty` now includes valid certificates. Using
it as the revoked-head/recovery guard requested revoked-head proofs before any
ending evidence arrived. `has_revocations` now distinguishes valid links from
signed revocations, void forks and over-bound links. Recovery uses that guard.
Revoked-head classification also uses the common `ends_chain` rule, so a fork
with no signed revocation can satisfy a pending dependency through verified
header evidence. The existing revoked-head regression now runs both an
identity revocation and a same-sequence membership fork, including refusing
premature evidence and resolving only after local ending evidence arrives.
Credential regressions assert that valid links are nonempty but not revocations,
and that merging a fork changes this distinction.


## Resumed bootstrap and consumer corrections

The two bootstrap hangs reproduced independently:
`join_progress_only_grows_to_its_total` and
`a_cut_join_resumes_its_pages_and_keeps_edits_from_both_sides`.
Server-side diagnostics identified `Rejected("package quota/generation")`:
row accounting reserved a fixed margin but omitted scoped escrow proofs
already present in the first page. The explicitly approved budget correction
counts the initial signed page metadata before adding rows. The additional
escrow regression fills pages with large rows and a maximum-sized escrow
payload, verifies encoded page and decrypted package bounds, imports every
page, and checks all rows and the original escrow payload survive. Final
results are recorded after the reruns below; the earlier timeouts are not
passing results.

Remaining consumer fixtures now provide application-owned signers at startup
and on branch/migration calls. The interrupted revocation intent names the
identity and capability, without the obsolete issuer-owned node target.
Author verification uses the node certificate's subject and author number.
The issuer surviving a root revocation receives independent root trust first,
so this test does not require a revoked delegation to remain authoritative.
Temporary diagnostics were removed.


Resumed focused consumer snapshot:
`/tmp/rocky-phase3-takeover-sdk-focused.log` reports 25 passes and one failed
assertion across 26 selected tests. The shared-list migrations, coordinator
migration, registry revocations and real-voter branch-log prerequisites all
pass. The interrupted-revocation fixture completes its operation, but its
assertion still indexed the old array encoding. It now reads `RevocationSet`
and compares the exact completed statement by its stable ID; the fresh rerun
is part of the checkpoint/bootstrap check below.


Fresh focused results after the approved page budget fix:

- `/tmp/rocky-phase3-takeover-bootstrap-checkpoints.log`: 68 passes across
  the checkpoint/no-frames, escrow, wire-budget, two bootstrap and interrupted
  revocation cases. Both formerly hanging bootstrap tests pass, and the
  completed-statement retry assertion passes. The one failure was the new
  size test using an employee name beyond that application's 200-byte limit,
  before checkpoint export; it is not counted as a pass.
- The size test now uses the existing SQL fixture, whose text field supports
  the eight 12 KB rows. Its isolated rerun passes
  (`/tmp/rocky-phase3-takeover-page-bounds.log`): every encoded page fits its
  requested quota, every decrypted package fits `MAX_PAYLOAD_BYTES`, all eight
  rows import, and the maximum-sized escrow payload decrypts unchanged.
- `./scripts/lint` exits 0 with the inherited `chaos.yml:86` expression
  diagnostic (`/tmp/rocky-phase3-takeover-lint-current.log`).
- The full required `./scripts/check` used `RUSTC_WRAPPER=` and the existing
  target. Its passing final result is recorded below
  (`/tmp/rocky-phase3-takeover-full-check.log`).


## Final gate record (2026-11-07)

`RUSTC_WRAPPER= ./scripts/check` exits 0:

- Native unit tier: 261 passed.
- Native integration tier: 1,172 passed, no failures.
- Workspace clippy and the SDK all-target check without default features pass.
- Wasm clippy passes.
- Browser tests: 71 of 71 runs pass, covering 903 selected tests in 68 binaries.
- Browser/native relay sync passes: the browser's write and coordinated claims
  arrive at the native node.

This final snapshot includes the scoped membership and two-database tests,
fork/relink and unlink/reader-rotation regressions, seed 10961 branch recovery,
current-format checkpoint vectors, both formerly hanging bootstrap tests,
the direct package/page-bound regression, and the corrected consumers.
It supersedes the earlier failed native/browser snapshots for this code.
Full output: `/tmp/rocky-phase3-takeover-full-check.log`.

`./scripts/lint` exits 0 while retaining the inherited
`.github/workflows/chaos.yml:86` expression diagnostic. Commitlint passes.
The independent checkpoint verifier checks all 28 frozen files
(`/tmp/rocky-phase3-takeover-vectors-python.log`).

The final preservation audit confirms all five protected hashes match.
Removing exactly the approved retained-row detection and initial page-metadata
accounting reproduces the protected transfer baseline byte-for-byte. All
work remains in the current jj change; no pushes, bookmarks, shared-history
rewrites, extra checkout or extra build cache were created.

These are the required unit/integration/browser gates, not the separate full
e2e/soak tier or desktop/distributed qualification. Existing ignored cases,
including Docker-dependent custody coverage, are not claimed as passes.
Phases 1, 2 (including offline identity ownership), 4 and 5a remain integrated.
Phase 5b code-confirmed linking/discovery and Phase 6 trusted-identity alerts,
staleness and complete lane cuts remain the next separate lanes. Applications
continue to own passkeys, QR, database selection and personal directories.
