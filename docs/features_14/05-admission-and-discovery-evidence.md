# Admission and discovery evidence

## Phase 5b current record (2026-11-07)

SDK implemented and verified; required workspace gate exits 0.
The historical Phase 5a results below are preserved. Their prerequisite
failures were superseded by the final Phase 3 gate in
[03-membership-evidence.md](03-membership-evidence.md#final-gate-record-2026-11-07).

Certified Seen discovery is available on any node. It intersects observed
contacts/live sessions/peer history with database-verified certificate evidence,
and reports the node, certifying identity, last-seen time, registry-known and
unknown flags, identity revocation and identity-owned unlink. Discovery hints
alone do not authenticate an identity or grant authority. Non-issuers expose
their missing registry knowledge explicitly.

Linking now pauses before sending the sealed bundle. The signed request binds
the database context, both nodes and fresh exchange nonce; its digest derives
the six-digit code displayed on each device. The answering device confirms via
`Session::confirm_link`; the joining device supplies the matching code and an
explicit per-call provider to `Configuration::confirm_link`. Its `AwaitingLink`
continuation contains request/exchange state, never the signer. Wrong codes or
missing confirmation refuse the bundle, and credentials are not installed.
Answering-side challenges expire after five minutes, keep at most 32 entries,
and preserve confirmation when the exact request retries. Phase 5a's persisted
pending-admission, invitation expiry, bound and retry behavior remain intact.

Regression coverage adds wrong-code refusal on either device, correct code
without answering confirmation, no installed credentials before both confirm,
matching displayed six-digit codes, and successful sequence-one installation.
Seen coverage exercises a non-issuer's unknown-identity flag, an issuer's known
identity, unlink and identity revocation. Existing three-node and independent
two-database flows now confirm both screens through the same public API.
Foreign link requests use the same reusable identity with distinct roots and
with a shared root/different scope. Rejection leaves the answering node's saved
settings and pending requests unchanged and installs no joining credentials;
subsequent valid links/unlinks and database-local grants still work. A direct
deterministic challenge test covers 33 arrivals, oldest eviction, exact retry,
confirmation expiry and no consumed membership sequence during challenge.

Final measured checks:

- Inherited focused run: 11 group-secret and 10 identity tests passed;
  `/tmp/rocky-phase5b-focused.log`.
- Extended focused run: all 12 identity tests passed, including code, Seen and
  foreign-context regressions; `/tmp/rocky-phase5b-identities-final.log`.
- `./scripts/lint`: exit 0 with the inherited `chaos.yml:86` expression diagnostic;
  `/tmp/rocky-phase5b-lint-final.log`.
- Required `RUSTC_WRAPPER= ./scripts/check`: exit 0. Workspace Clippy and the
  SDK all-target compile without default features pass. Native unit tier:
  261 passed; integration tier: 1,175 passed, no failures. Wasm Clippy passes.
  Browser tier: all 71 runs pass, covering 903 selected tests in 68 binaries.
  The browser/native relay test passes, including browser writes and
  coordinated claims arriving at the native node. Full output:
  `/tmp/rocky-phase5b-full-check.log`.
- `scripts/commitlint --jj`: passed; `/tmp/rocky-phase5b-commitlint.log`.

The initial full gate stopped on a needless borrow in the new dispatch arm;
that was corrected before the passing final run. The focused identity run
also reported an unused close result; the final code propagates it and passes
Clippy. These gates validate the SDK and existing browser/native probe, not
Paperless, the full e2e/soak tier, a desktop application or general distributed
qualification. The new interactive linking regressions run natively over the
memory transport; browser compilation and the existing browser matrix pass.
Ignored cases are not claimed as passes.

`cargo metadata --no-deps --format-version 1` establishes `rocky-credentials`
as request/code owner and `rocky-sdk` as lifecycle, discovery and consumer owner.
Paperless (`paperless-core`, `paperless-node`, `DatabaseSettings.tsx` and
`native/join_approval.rs`) was searched in the checkout; the listed sibling
projects contain no Paperless checkout. Its owner-approval/UI integration is
unavailable external consumer work, not a tested or completed change here.
Applications still own passkeys, QR, database selection and encrypted personal
directories with bootstrap/sync/recovery. Phase 6 is next; Phase 7 was not edited.

The six protected files were checked against
`/tmp/rocky-phase5b-protected-start.sha256`; all six match byte-for-byte.
Final audit output: `/tmp/rocky-phase5b-protected-final.log`. The unrelated Phase 7
evidence was separately snapshotted and preserved byte-for-byte: an external
process moved it from `features_14` to `features_15` during verification; its
SHA-256 remains `835267397b2f432039a80f519d126c5f0495dfaa3c2ad1729b57388cade3bce5`.
That process also updates the roadmap and runs its own check; this lane did not
move those files or stop that process. Work stays in current jj
change `lzmuuprq`, parent `qouwotvn` (`0d031c15`), without commits, bookmarks,
pushes, history rewrites, other checkouts or extra cache/build directories.

## Historical Phase 5a record

Status: Phase 5a implemented; required gates run with the limitations below (2026-11-06).

The change duplicates `lane/f14-admission@origin` at `4743ab255fa4` onto main
`cec2ad1d78393e4ebee6366fefa6dda9f44df8ed`. Enrollment uses Phase 1 node-signed
authority and Phase 4 identity-keyed records, bounds and activity names.
Pending admission preserves the current main identity-provider behavior;
Phase 5a does not add membership or change identity-key retention.

The three reported integration regressions are corrected: peers refuse every
non-current invitation version (including v5), explicit profiles still bound
reads and writes while the absent/null profile opens an approval invitation,
and custody routing uses invitation v6 while retaining the issuer node key.
The supplied-credentials fixture in that focused suite now signs through the
creator's certified node, as Phase 1 requires.

Regression coverage includes matching and mismatched codes, bounded oldest
request eviction, invitation replacement and expiry, refusal, issuer restarts,
identity labels, lost answers at both sides of the exchange, and revocation
before an admitted device retries. Each answered request retains its author
range; registry capacity is checked again before issuing an enrollment.

Verification:

- `cargo check --workspace --tests`: passed.
- `cargo check --manifest-path benchmarks/chaos/Cargo.toml --tests --locked`:
  passed. The benchmark's separate lockfile now includes `bip39` and its two
  missing transitive packages, required by the merged Phase 2a dependency.
- `cargo test -p rocky-credentials`: passed, 93 tests including the compile-fail
  identity-key doctest.
- `cargo test -p rocky-sdk --lib`: passed, 201 tests, 22 ignored.
- Focused SDK credentials and custody suites: passed, 6 and 4 tests
  respectively (one custody test is ignored).
- The two registry fault-injection regressions: passed; both sides of a lost
  answer retain the same identity, and a revoked admitted identity is refused.
- Workspace Clippy and the SDK without default features (`--all-targets`):
  passed as stages of `./scripts/check`.
- `./scripts/check --unit`: 240 passed, 16 coordinator tests failed. These
  unchanged authority fixtures fail in `machine/coordinated_tests.rs:87`
  (`Denied`) and `machine/retention_tests.rs` at lines 152, 183 and 286
  (`Denied`, including an expected `Backpressure`). The runner also
  reports the failed binary's exit 101; that is not another failed test.
- `./scripts/lint`: exit 0, with the existing actionlint diagnostic at
  `.github/workflows/chaos.yml:86:69` for arithmetic in a GitHub expression.
  The lint wrapper continues through its workflow regression checks and
  returns success despite that diagnostic. Formatting, shellcheck and the
  script regression checks pass.
- `./scripts/commitlint --jj 'main@origin..@'`: passed.
- `cargo test -p rocky-sdk --test peers`: passed, 7 tests after limiting the
  foreign-node metadata scan to data outside certified credential chains and
  signed discovery endorsements.
- Final `cargo clippy --workspace --all-targets --locked -- -D warnings`:
  passed after the peers fixture update.

The native part of `./scripts/check` completed with 239 unit tests passed,
17 failed, and 981 integration tests passed, 140 failed and 27 missing. Its
unit run includes the same 16 coordinator failures plus
`phrase_key::tests::a_phrase_restores_the_same_key_and_a_wrong_phrase_is_refused`:
its randomly replaced word happened to form another valid-checksum phrase.
That unchanged Phase 2a test passed in the standalone unit and credentials
runs. The integration run contains the intermediate peers scan failure,
subsequently corrected and verified by the seven-test rerun above.

Native integration limitations:

| Binary | Failed | Missing |
| --- | ---: | ---: |
| `rocky-coordinator` | 28 | 0 |
| `rocky-secure` | 100 | 0 |
| `rocky-coordinator::public` | 2 | 0 |
| `rocky-coordinator::cluster` | 1 | 0 |
| `rocky-sdk::migrations` | 1 | 0 |
| `rocky-sdk::registry_revocation` | 2 | 0 |
| `rocky-sdk::concurrent_rotation` | 1 | 0 |
| `rocky-sdk::coordinated` | 1 | 0 |
| `rocky-sdk::service` | 2 | 0 |
| `todo-desktop` | 1 | 0 |
| `rocky-sdk::peers` (since corrected) | 1 | 0 |
| `rocky-sdk::tips` | 0 | 27 |

Coordinator and secure failures predominantly reject old identity-signed or
grant-bearing node fixtures (`invalid node certificate`, `not a node
certificate`, `an issuer signs through its node`, or `Denied`). The SDK
migration test expects an unavailable identity provider to prevent routine
node signing. Registry-revocation tests encounter cascading origin revocation
and an identity-only credential in a node session, as Phase 4 evidence notes.
Concurrent rotation still has a row-visibility assertion failure. Coordinated
and service tests reject a node as untrusted; the desktop fixture rejects an
invalid node certificate. The tips batch hits the runner's 120-second hang
stop in `coordinated::a_seal_another_node_ended_first_fails_sealed`, and the
runner reports all 27 batch results missing despite partial output.
These failures are not claimed to be resolved by Phase 5a.

The wasm stage of `./scripts/check` built successfully and passed Clippy. It
selected 888 tests from 67 binaries in 69 browser runs: 49 runs passed and 20
failed. The failures are confined to `rocky-secure` (84 tests) and
`rocky-coordinator` (41 tests), with the same legacy certificate/authority
rejections. The SDK and credentials browser runs passed, including the
pending-request bounds and code checks.

The browser/native sync probe compiled but failed before browser sync: the
native host could not follow `main`, reporting `not a node of a trusted
identity of this domain`. The full wrapper exits 1. This is not evidence of a
working distributed protocol or desktop app. The e2e/soak tier and runtime
tests of the SDK without default features were not run; their binaries were
checked, and focused native and browser tests exercised Phase 5a.
