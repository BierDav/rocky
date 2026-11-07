# Phase 5a admission evidence

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
