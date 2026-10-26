# Rust application SDK

Status: implemented; automated Rust and Kotlin checks passed (2026-10-26).

The public `rocky` package lives in `sdks/native` and belongs to the Cargo
workspace. The former native binding package lives in `sdks/uniffi` and depends
on `rocky`, so Kotlin and Rust share the same implementation in `rocky-sdk`.
Kotlin build inputs, packaging, qualification commands and CI paths follow the
binding package; its ABI and library names remain `rocky_uniffi`.

The Rust API provides validated schema builders, reusable async configuration
for explicit create/open/join, optional owned or borrowed networking and
parameterized SQL without native frames or statement identifiers. Transactions
own their connections, refuse manual transaction control, roll back on drop and
wait for local durability on commit. They never retry application work.
Cancellation during commit can leave the outcome uncertain. Bindings retain
their existing buffered commit policy.

Paged reads keep snapshot connections, owned rows decode columns strictly by
name or position, and ambiguous names require aliases or positional access.
Nullable values require `Option<T>`. Parameters bind typed values; REAL values
use the existing explicit NaN-rejecting constructor. Table observation and
status/sync APIs use the existing shared runtime. Pairing takes an explicit grant
profile; contacts and peer configuration have typed Rust methods.

Cleanup has a separate reactor channel so a full request queue cannot silently
discard connection rollback or session cleanup. Cancelled connection acquisition
and statement responses release their resources. No ORM integration was added.
The new table builder exposes replicated tables; local-only domain support is
outside this change.

The `notes` executable and crate-level example show application setup, managed
IDs, writes, observation, reads and awaited shutdown. The public-package tests
exercise persistence, rollback, task cancellation, snapshot paging, decoding,
explicit enrollment and two-peer replication without JSON calls.

## Qualification

- `./scripts/lint`, `./scripts/commitlint --jj` and the final
  `cargo clippy --workspace --all-targets --locked -- -D warnings` passed.
- The workspace unit tier passed all 203 tests. The integration tier passed all
  1,419 tests with `NEXTEST_TEST_THREADS=4 ./scripts/check --integration`, in
  270.92 s against its unchanged 300 s budget.
- The first full `./scripts/check` run passed 1,418 integration tests but failed
  the pre-existing secure coordinated crash test at its 20 ms sleep/clock advance
  (`coordinated.rs:1009`, `Aborted`). It passed alone in 0.714 s and in the full
  integration rerun. No timeout, admission rule or tier budget was relaxed.
- The public package's six application tests passed without default features
  (0.77 s together) and in the workspace's default feature build. The crate-level
  example compiled as a doctest. All application targets also passed
  `cargo check -p rocky --all-targets --no-default-features`.
- `./sdks/uniffi/check` passed: formatting, Clippy, the binding generator, tests,
  and the binding build without default features.
- `sdks/kotlin/gradlew -p sdks/kotlin check --continue --no-parallel
  --max-workers=2` passed: JVM and iOS simulator tests, SQLite/Room checks and
  the artifact-only packaged JVM consumer. The simulator native library also
  rebuilt successfully through the new public SDK dependency.

Physical-device sync, shopping application packaging, and performance phases
are not qualified by this work.
