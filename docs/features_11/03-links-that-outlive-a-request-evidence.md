# 3. Links that outlive a request: evidence

Status: implemented (2026-10-25); qualification incomplete. Workspace:
`~/AgentWorkspaces/rocky-links`. Its baseline is an isolated snapshot of the
in-progress phase 1 implementation; the original checkout is unchanged.

## Changes

- Dropped and timed-out channel generations retire their receivers and partial
  messages. Late fragments are counted and discarded until the peer's `End`;
  live full queues still reject quota violations. Slots retain their generation
  until both ends finish, and link shutdown clears them.
- All request families share a signed `Refused { reason }` response with five
  reasons. Serving emits channel `Working` packets every four seconds while
  preparing an answer. Channel progress renews the ten-second idle deadline;
  a request has a 120-second lifetime, including sending. Handler errors leave
  the authenticated link available for further requests.
- Sync records failed scopes, continues the remaining scopes and retires the
  failed request channel. Successful scopes reuse their channel. Recovery of
  wanted seals is scoped too. Bootstrap planning and importing continue after
  a scope failure, retain unfinished intents and exclude importing scopes from
  ordinary row reconciliation. Status records scope outcomes and peer-cleared
  `SyncRequest` blockers.
- Push/signal overload marks the peer pending and drops the excess work.
  Receiving pushes does not acquire the page-publication mutex. Deterministic
  test gates cover a burst beyond the eight-handler limit and catch-up afterward.
- Tip queries, pushes, handshake statements and pending candidate batches split
  by encoded size. Candidate pages retain their batch boundary so dependencies
  still verify together. Frontier requests and responses cut independently and
  resume the unreturned prefix. Holdings proofs and existence pages use byte
  budgets too. The payload budget is 258,048 bytes (256 KiB minus 4 KiB), with
  additional family/cursor room where needed. Oversized individual tip and
  existence records fail before storage.
- Terminal acknowledgments retain their 16 KiB authoring bound. Finality now
  permits 15 acknowledgments, yielding `MAX_FINALITY_BYTES = 246,784`, leaving
  room for the statement, family envelope and signature. The encoded-size
  regression measures **15,864 bytes** for an eight-link fixture at the largest
  whole-grant count accepted by that acknowledgment limit. It measures encoding only; trusted
  finality still requires the existing owner-direct trusted identity/node chain.
- Retry delays have deterministic ±25% jitter from a per-node seed mixed with
  node and peer identities. Reopening a link and progress wakeups preserve the
  failure count. A completed sync pass or a link surviving 60 seconds resets it.
- The unused subscription pull protocol and per-frame acknowledgment messages
  are removed. Their tests and the desktop demo now use frontier-based
  `catch_up`. Link format is **17**; row, checkpoint and replica formats stay
  unchanged.

## Verification

- `./scripts/lint`: passed, including Kotlin lint using the repository's pinned
  `ktlint` installed by `scripts/tools ktlint`.
- `./scripts/commitlint --jj`: passed. The current change is
  `fix(sync)!: keep links alive across request failures`; its breaking-change
  footer names link version 17 and the 15-acknowledgment finality cap.
- `sdks/native/check`: passed formatting, Clippy with `bindgen`, the native
  binding test and the build without default features.
- Encoded-size regression: all four family-bound tests passed. The maximum
  whole-grant eight-link fixture measured 15,864 bytes per acknowledgment;
  the finality bound is 246,784 bytes against the 258,048-byte payload budget.
- Targeted fault regressions passed for a late reply after timeout, retired
  channel drain, generation reuse, progress and the 120-second lifetime,
  same-channel refusal, scope isolation, push publication-lock independence,
  handler overload/catch-up, jitter, bootstrap resumption and the desktop demo.
- Final `./scripts/check`: Clippy passed; **196 unit tests passed**. The
  workspace integration run passed **1,035 of 1,036** tests. The SDK build
  without default features passed **287 of 287** integration tests. The
  combined integration tier passed 1,322 tests in 214.86 seconds, within its
  300-second suite budget; individual over-budget results passed isolated
  reruns. The command exits 1 solely for the baseline failure below.

The workspace check has a reproduced baseline failure:
`tests::tips::frontiers::headers::excluded_and_other_domain_headers_still_decode_after_warming`.
The fixture deliberately corrupts a stored header and expects every query to
reject it. An index-backed metadata lookup returns success instead. The same
case fails in the original checkout's pre-existing test binary
(`rocky_secure-7a3b5a6cce181afb`, 545 tests), as well as this workspace's binary.
The failure is reported; its rejection assertions remain intact. The test's
failure output now identifies the unexpected result. No position-index or
stored-header reader behavior is changed in this phase.

## Not verified

- Phase 2's seeded multi-node simulator is not present in this baseline. The
  paused-clock and deterministic handler-gate regressions cover timeout,
  progress, refusal and overload schedules; they are not a seeded simulation
  sweep with cuts on every link.
- The required 30-minute shopping run on two desktops and one Wi-Fi phone and
  the isolated three-device 100k load have not been performed. A physical Pixel
  phone is connected, but no new phase 3 application build was installed or
  qualified during this implementation. No device build names, identities or
  link-closure measurements are claimed.
