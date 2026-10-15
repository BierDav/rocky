# Development workflow

## Tooling

Use rustup, sccache, Jujutsu (`jj`) and ripgrep (`rg`). `rust-toolchain.toml` pins Rust
1.99.0 with rustfmt and Clippy. Invoking Cargo in the repository selects it.
Workspace crates inherit shared dependency and lint configuration.

Keep the root `Cargo.lock` committed. Update it intentionally when changing
dependencies. The engine benchmark retains its own manifest and lockfile.

### Disk use

Each Jujutsu workspace builds into its own `target` directories, so a full
build costs the same space in every worktree. The `dev` profile keeps only
line-table debug info and no incremental state (see the profile comment in
`Cargo.toml`); restore full debug or incremental with `CARGO_PROFILE_DEV_DEBUG=2`
or `CARGO_INCREMENTAL=1`. The repository's `.cargo/config.toml` names `sccache`
as `rustc-wrapper`, so dependency crates compile once and are reused across
worktrees; `sccache` must be on `PATH`, and `RUSTC_WRAPPER=` (empty) builds
without it, as the container images do. `SCCACHE_DIR` selects the cache. `scripts/clean` removes one checkout's Cargo and Kotlin/Gradle
output, and `scripts/clean --evidence` also removes generated benchmark
databases; run it in each worktree to reclaim space.

## Version control: Jujutsu only

Use `jj` for all version-control operations. Never invoke the Git CLI, including
read-only commands. `jj git` is Jujutsu's supported Git-backed remote transport.

```sh
jj status
jj diff
jj log -r 'ancestors(@, 5)'
jj file list
```

Jujutsu snapshots working-copy changes automatically; there is no staging step.
Keep related edits in one change. Review the diff and check results before
describing or separating changes. Preserve work outside your task.

```sh
jj describe -m "fix(rocky-runtime): describe the concrete change"
jj new
```

Descriptions follow [Conventional Commits](commits.md); `scripts/commitlint --jj`
checks them, since Jujutsu runs no Git hooks.
Run `scripts/setup` once per clone for the `jj fix`, `jj lint` and `jj push`
configuration and the pinned lint tools ([linting](linting.md)).

Use these commands when intentionally finishing a change and starting another.
Agents normally leave their edits in the current change for review.

Fetch through `jj git fetch`; publish an explicitly selected bookmark through
`jj push --bookmark <name>` only when publication is requested. It checks the
commits it would publish before running `jj git push`. Do not create bookmarks
or rewrite shared history as an incidental part of local editing.

## Shared checks

```sh
./scripts/lint
./scripts/check
```

`scripts/lint` checks Rust and Kotlin formatting, shell scripts and workflows
without a build; `--fix` formats first. [Linting](linting.md) lists its tools.
ktlint's standard and experimental rules include unused-import checks; the
wildcard import allowlist in `.editorconfig` is limited to extension-heavy
project dependencies.

`scripts/check` resolves the repository from its own location. It runs formatting,
workspace Clippy with `clippy::all` denied and warnings treated as errors, and
the unit and integration [test tiers](testing.md). The doc tests are opt-in
with `--doc` and no CI job passes it. Dependency resolution uses the committed
lockfile. It needs [cargo-nextest](https://nexte.st) and Python 3.11 or newer.

During development, run the unit tier, or narrow verification to the package
you changed:

```sh
./scripts/check --unit
cargo test -p rocky-runtime --locked
cargo clippy -p rocky-runtime --all-targets --locked -- -D warnings
cargo fmt --all
```

`./scripts/check --unit` builds on [Cranelift](testing.md#cranelift) once
`scripts/cranelift install` has run. `./scripts/check --e2e` runs the end-to-end
tier, which only CI runs otherwise; soak tests are part of it.

Run the shared checks before completing workspace changes. Introduce explicit
checks for supported feature combinations when platform features are added.
Do not assume `--all-features` represents a valid platform configuration.
`./scripts/check` also tests `rocky-sdk` without its default `iroh` feature,
and `./sdks/native/check` builds the ABI without it.

### Test budgets

The per-tier budgets, how they are measured and how to fix a miss are in
[testing](testing.md); [decision 0060](decisions/0060-test-tiers-and-budgets.md)
records why. Measured on an 18-core Apple M5 Pro shared with other builds; the
runs and their load are in the [tier evidence](features_07/01-test-tiers-evidence.md)
and the earlier [phase 1 evidence](features_07/01-test-speed-evidence.md). A
change that misses a budget names the cause and its largest test, or makes it
faster.

| Budget | Checked by | Result |
| --- | --- | --- |
| A unit test: 10 ms alone, in process | `./scripts/check`, `--unit` | 132 tests; the slowest 7.9 ms in the run |
| The unit tier: 10 s | the same | 0.09 to 0.12 s |
| An integration test: 2 s alone | `./scripts/check`, `--integration` | 682 tests; the slowest 1.52 s alone |
| The integration tier: 5 min | the same | 92 to 98 s |
| The e2e tier: 15 min | `./scripts/check --e2e`, the CI `e2e` job | 55 tests in 272 s locally; CI not measured |
| `./scripts/check` with a warm build: 6 min | `./scripts/check` | 249 s |
| The edit loop after touching `rocky-core`: no budget | `./scripts/check --unit` | 11 s on Cranelift, 33 to 38 s on LLVM |
| `sdks/kotlin/gradlew -p sdks/kotlin check` with a warm build: 4 min | that command | 95 s |
| `./examples/shoppinglist-app/check` with a warm build: 3 min | that command | 60 s |
| `./sdks/native/check` with a warm build: 30 s | that command | 6 s |
| `./scripts/lint`: 30 s | `./scripts/lint` | 25 s beside a full check |
| CI from push to a green `check` with a restored cache: 30 min; `lint`: 5 min | the hosted run | not yet measured |

Phase 8's pure identity-order property and group capacity test run in unit.
Sibling history, exposure, renewal, hub convergence and the deterministic
recovery boundaries run in integration through the existing package filters.
The scaling SQLite-header probe also runs in integration because it creates
and reads a real file.
The recovery sweep copies one prepared database instead of repeatedly creating
identities; the hub fixtures use an in-memory secret provider. They receive the
same 10 ms/2 s budgets, with no new exception. Measurements and coverage are in
[the phase 8 evidence](features_07/08-concurrent-rotation-evidence.md).

Phase 10's pure holding-proof binding test stays in unit. Its authenticated
SQLite privacy, replay, retained-epoch and custody scenarios run in integration
through the `network::holdings::tests` filter. The full-size four-table,
200-partition idle proof gate runs in e2e and asserts the 10 s phase budget.
No new test-budget exception is added. See the
[phase 10 evidence](features_07/10-holdings-evidence.md).
The existing owned-iroh restart and executing-device service scenarios also run
in e2e: their real QUIC shutdown cycles exceed 2 s after fixture optimization.
The deterministic revocation recovery sweep remains in integration, separated
by secret, table and commit boundaries with the same twenty fault stops.

Phase 11's chain tests (`tests::chains` in `rocky-secure`) run in integration
through the existing package filter: authenticated in-memory sessions and real
SQLite replicas, the slowest 0.35 s alone. No exception is added. The
four-table and single-table fixtures stay ignored measurements; their rerun is
in the [chain evidence](../benchmarks/runtime-scaling/evidence/chains-2026-10-10/README.md).

Phase 12's sealing and receipt checks in `rocky-credentials` run in unit. The
delivery family over a real session (`rocky-secure` `tests::delivery`), the
SDK's delivery, mailbox and revocation scenarios (`rocky-sdk`
`node::delivery_tests`) and the migrated sharing
tests run in integration through the existing package filters, with no new
exception. The nine crash-sweep cases and the mailbox's full-size
4,096-deposit bound run in e2e through explicit filters. The
node-level bound fills a saved mailbox and reopens it rather than sealing
thousands of envelopes over sessions. Joined fixtures wait until the join's
first sync has finished, so no pass in flight meets a later share.
The null-ordering pagination test in `rocky-runtime` opens a SQLite file and
runs a Tokio runtime; it is explicitly classified as integration instead of
unit. This corrects its tier without raising any budget.

Phase 13's signed-policy checks run in the credentials integration target. The
`rocky-secure` `tests::partition_foreign_keys::horizon::relay` scenarios run
in integration through the existing package filter, as does the SDK horizon
surface regression. The full shopping hub gate remains e2e; the Kotlin
shopping harness runs in the existing shopping CI job. No exception is added.
See [Phase 13 evidence](features_07/13-relayed-horizon-evidence.md).

Phase 14's secret-store topic/eviction tests and in-process contact, ranking,
request, refusal and custody scenarios run in integration. The `mesh` and
`network::contacts::tests` modules are included explicitly in
`.config/test-tiers.toml`; no exception is added.
[Phase 14 evidence](features_07/14-contacts-and-partner-choice-evidence.md)
records measurements and the acceptance coverage.


Phase 15's record, sealing and merge checks in `rocky-iroh` run in unit. Its
transport tests (`rocky-iroh` `discovery`), the hub scenarios
(`rocky-sdk` `node::sync_tests::discovery`), the relay run and the
attached-endpoint check (`rocky-sdk` `discovery`) run in integration through
the existing filters. The mDNS transport run and the two two-process
acceptance runs are named in e2e with their measured times; no exception is
added. The simulator and emulator gates are scripts, as for phases 17 and 18.
See the [phase 15 evidence](features_07/15-discovery-evidence.md).

Phase 16's forwarding-slot and carrier-signature tests run in unit, explicitly
excluded from the SDK's integration module filter. Mesh routing and replay tests exercise
SQLite and real authenticated carriers and run in integration through
`kind(test)`. The forwarder-restart case runs in e2e because it exercises
the production 5 s service drain. No budget exception is added. See
[Phase 16 evidence](features_07/16-strong-commands-evidence.md).

Phase 20's Room schema, missing-table, registry-name, scanned-contact sharing
and contact-handover tests belong to Kotlin integration in `shared`; the
multi-process shopping scenarios and two-emulator procedure belong to e2e in
the shopping CI job. `scripts/test-tiers` tiers Rust tests only; no Rust test
filter or budget exception is needed for Kotlin application tests. The native
shopping release regression remains in Rust e2e with read-only member list
grants. The driver projection and secure horizon custody regressions use SQLite
and sessions and belong to Rust integration through the existing filters.
Measurements and remaining failures are recorded in the phase evidence. The literal
member-directory byte audit remains inside the full `gate` scenario. See
[Phase 20 evidence](features_07/20-shopping-without-a-roster-evidence.md).


## Kotlin desktop integration

The SDK is a Gradle build under `sdks/kotlin`. `sdks/kotlin/gradlew -p sdks/kotlin check`
runs every SDK check and test. Android is packaged and qualified separately; see
[below](#android). The SDK's build, packaging and qualification tasks are
[Gradle convention plugins and task types](../sdks/kotlin/README.md#build).
The shopping-list application is a standalone Kotlin toolchain project: its
`project.yaml` workspace has Amper modules that configure JVM desktop targets.
Run `./examples/shoppinglist-app/check` for the actual Room/Rocky integration
and desktop/Android app compilation, `./examples/shoppinglist-app/qualify` for the
multi-process [shopping qualification](runtime/shopping-qualification.md), and
`sdks/kotlin/gradlew -p sdks/kotlin :qualification:inventory:check`
for the existing SDK contract qualification. Run `./sdks/native/check` for the
excluded UniFFI ABI crate. Run `sdks/kotlin/gradlew -p sdks/kotlin :qualification:packaged-jvm:qualifyPackage`
for an artifact-only JVM consumer, or `:qualification:packaged-jvm:packageSdk` to
produce the standalone library/resources and example in `sdks/kotlin/qualification/packaged-jvm/build/packaged-sdk`.
[Packaging](runtime/sdk-packaging.md) documents no-build-tools consumption.
Run `sdks/kotlin/gradlew -p sdks/kotlin :rocky-sqlite:jvmTest :qualification:room:jvmTest` for the
[SQL driver](runtime/sql-driver.md) and its pinned Room fixture. The Room gate
uses the standalone `sqlite3` CLI with JSON output for read-only inspection of
the live file; install it before running that gate.
Run `sdks/kotlin/gradlew -p sdks/kotlin :qualification:schema:check` for hand/Room definition
equivalence and native installation, and `:qualification:schema:jvmTest` to
regenerate the pinned Room exports and check compound-key scalar DAOs.
`sdks/kotlin/gradlew -p sdks/kotlin check` runs the SDK's Gradle checks and tests; it is not a
formatter or linter. Kotlin formatting and lint run in `./scripts/lint`.
Every build prepares SDK-owned native bindings/resources with Cargo, so it needs
the pinned Rust toolchain. These are separate from the Rust `./scripts/check` gate.
See [Room on Rocky's file](runtime/room-integration.md).

## iOS bindings

On macOS/Xcode, run `./sdks/native/check-ios` for both arm64 runtime targets
with and without iroh. `./sdks/native/qualify-ios-binding` generates and tests
the isolated binding's callbacks over loopback iroh on the simulator. `sdks/kotlin/gradlew -p sdks/kotlin -Procky.qualification=true iosSimulatorArm64Test`
runs the common SDK/driver/Room tests and the endpoint, command, cancellation
and lifetime gates with iroh. [The binding guide](runtime/kotlin-native-binding.md)
records the toolchain, dependency repair, threading contract and evidence.

Phase 18 adds `sdks/kotlin/gradlew -p sdks/kotlin packageIos` and
`... qualifyIosPackage` for the independent packaged consumer, and
`./sdks/native/qualify-ios-lifecycle` for secret handling, suspension and crash probes.
Phase 15 adds `./sdks/native/qualify-ios-discovery`: local discovery follows
the lifecycle, and a simulator and a host process find each other over mDNS.
The new Rust endpoint ownership/rebind tests are integration tests through
`kind(test)` in `.config/test-tiers.toml`, with the same 2 s budget. The iOS
acceptance tests run in the macOS job; they are platform integration gates,
not additions to the Rust unit tier. The lifecycle gate enforces 2 s for each
of its seven tests; the packaged consumer enforces the same budget. See
[iOS](runtime/ios.md).

Phase 21 adds `./examples/shoppinglist-app/check-ios`: common pairing tests on
Kotlin/Native, real Room shopping persistence and native command/ticket
regressions, and both simulator app builds. `IosShoppingTest` is a
platform integration test and enforces the same 2 s budget in its body.
The `qualify-ios` agent-device procedure is e2e and enforces the 15 min suite
budget excluding build. These are outside `scripts/test-tiers`' Rust/nextest
catalog; `.config/test-tiers.toml` records that boundary, with no new exception.
The macOS CI job runs both gates. See [Phase 21 evidence](features_07/21-ios-applications-evidence.md).

## Android

The [Android runtime](runtime/android.md) is the `rocky-runtime-android` library of the
SDK Gradle build; its Cargo task builds the native libraries, and `rocky-core`'s Android
variant depends on it. It needs the Android SDK and NDK and rustup, which it provisions
when none is on `PATH`. `sdks/kotlin/gradlew -p sdks/kotlin publishToMavenLocal` publishes
the Android variants for Gradle consumers.
This is not part of `./scripts/check`. See [Android](runtime/android.md).
`sdks/kotlin/gradlew -p sdks/kotlin :qualification:android-discovery:qualify` installs a small check app on a
booted emulator, or boots the AVD named by `-Procky.android.avd=<name>`, and shows that
the Wi-Fi multicast lock is held while local discovery is on and released in
the background.

## CI

The baseline workflow runs for pushes to the default branch and for the release
workflow's call, not for pull requests. Its `lint` job runs `scripts/tools` and
`scripts/lint` on Linux; it needs no build and fails within minutes. The
`build`, `check`, `e2e`
and `shopping` jobs also run on Linux. The `ios` job uses an arm64 macOS host
for the static target matrix and simulator checks.
The `build`, `check`, `e2e` and `shopping` jobs use the x64 `ubuntu-latest`
runner on both forges, so they can share nextest archives and the custody peer.
On Gitea, `lint` uses `ubuntu-latest-any-arch`.
The `build` job compiles the baseline workspace. It downloads an archive of
the exact event revision and runs `scripts/check --build`: strict workspace
Clippy and the test binaries as nextest archives. The workspace test build
already compiles the custody peer; it copies that binary before the SDK build
without default features can replace it, avoiding a separate peer compilation. It uploads three independent artifacts -- `nextest-workspace`,
`nextest-sdk` and `rocky-peer` -- so a failure to finalize one cannot also lose
the others, and each consumer fetches only what it runs.
CI disables the local Cranelift shortcut and uses the pinned LLVM toolchain.
Linux jobs install the pinned Rust toolchain with `dtolnay/rust-toolchain`; the
workflow names the same version as `rust-toolchain.toml`, which `rustup` still
resolves inside the checkout. The iOS job reads that file for its official
rustup toolchain and target installation. Rustup bootstrapping and nextest
downloads retry TLS resets, and extraction starts only after a complete download.
The bootstrap also registers Cargo and Homebrew tool paths explicitly.
The build shares its nextest binary as `nextest-tools`, so test consumers avoid
separate external downloads and use the same runner version as the archives.
They install it themselves only when that artifact is missing. The fallback
GitHub installer is pinned to a commit to avoid Gitea action-cache races when its
tag moves. Gitea installs the runner-architecture nextest
with `scripts/ci/install-nextest`; GitHub uses `taiki-e/install-action`.
The Gitea installer uses a pinned GitHub release directly, caches the binary
alongside Cargo registry data, and falls back to a locked source installation
when release downloads fail.
Before archiving, `scripts/ci/test-profile` optimizes the CPU-heavy workspace
test crates, including the secure holding-proof path, while retaining debug
assertions and overflow checks. Artifact
fallback builds use the same test profile. Production profiles and test budgets
are unchanged.

`check` downloads both test archives and runs `scripts/check --from` them:
`cargo fmt --all -- --check` and the unit and integration tiers, without
compiling. `e2e` downloads both for the end-to-end tier. `shopping` downloads
only the x64 `rocky-peer`; `android` needs no Rust artifact and keeps its own
Gradle and native caches. The downloads
are best-effort: Gitea's artifact service sometimes fails to finalize an
artifact, and a job that finds one missing rebuilds the test binaries from the
dependency cache (`check` runs `scripts/check --no-clippy` so it does not repeat
the Clippy the `build` job already ran). `check` and `e2e` compile nothing
otherwise, so they restore the Rust cache only then.
It needs no version-control CLI or repository history, and never invokes Git.
Android and iOS build their own platform artifacts and start alongside the Linux
build. Package-index requests time out after 15 seconds so an unrelated vendor
feed cannot hold up system dependency installation.
`scripts/ci/install-system-deps` installs the libraries a bare runner image
lacks: `libclang` for bundled SQLite's bindgen build, and the GTK/WebKitGTK,
X11, `libxdo` and OpenSSL development packages the Dioxus desktop example and
the websocket transport link against.

A job that passes records a cache marker keyed by the source tree. A later run
of the same tree, such as a redescribed or rebased commit with unchanged content
or the release of an already-checked commit, finds the marker and skips its
steps. Any change to a tracked file, including the workflow, runs it again.
When `build` finds `check` already passed for the tree it builds nothing, and
the jobs after it build what they still run themselves. One Rust cache holds
Cargo downloads and cleaned compiled crates. A source-tree comment in the generated
Cargo configuration refreshes its save key for every tree while retaining the
compatible toolchain restore prefix. The build job saves it; shopping saves its
separate cache for its Kotlin and native compilation. Consumers restore it only
when they need to compile.

Gitea archives stamp every file with the commit time. `scripts/ci/source-times`
records content hashes and timestamps in the Rust cache's registry directory.
After restoring the cache it preserves a file's previous timestamp only when
its contents match; changed or new files receive a fresh timestamp, including
when a rebased archive is older than cached outputs. Cargo still checks its
normal compiler, configuration and dependency fingerprints. The regression check
`scripts/test-source-times` verifies reuse and changed-source rebuilds with Cargo.
The same normalization follows the Android native cache restore in Baseline,
shopping and release jobs, keeping their shared compiled inputs reusable.
This avoids compiling unchanged workspace crates on every push. The additional
raw `target` overlay was removed: its 8 GB extraction repeatedly terminated Gitea
build runners and duplicated the Rust cache. Gitea's Kubernetes runners must
share persistent cache storage so another runner or a restarted pod can reuse it.

The `e2e` job runs `./scripts/check --e2e` on pushes to `main`; releases do not
wait for it.
The shopping job restores the Android job's Gradle toolchain cache. Cold Gradle
distribution downloads retry transient network failures. The artifact-only iOS
consumer retains `DEVELOPER_DIR` when restricting its environment to non-Rust
tools, so a runner's selected full Xcode remains available. iOS keeps compiled
workspace crates in its Rust cache even when a later qualification fails.
The iOS cache excludes Cargo's bin directory so cache cleanup leaves the host's
rustup executable and its Cargo proxies intact.
On macOS hosts without zstd, GNU tar and pigz compress that cache in parallel
while retaining compatibility with the existing gzip cache. When Homebrew cannot
install these optional tools, the host uses system gzip. Gitea's macOS checkout moves to a stable,
job-owned path under `~/.cache/rocky-ci/<owner>/<repo>/ios/source`; native and Gradle build
outputs survive replacement of the source snapshot. This avoids changing Cargo
package identities and cache paths on every host-executor run. Its Java tool
cache also uses a stable path, allowing Gradle daemons to reuse the installed JDK.
iOS jobs serialize per repository because refs share these outputs.
The source hash
checks and tree-based cache refresh also apply to iOS. Cold Linux and iOS jobs
have 90 minutes for initial cache seeding and artifact saves; test-tier budgets
remain unchanged.
Linux shopping checks publish only the common metadata, JVM and Android
artifacts to Maven local. Disabled Kotlin/Native targets do not trigger Rust
iOS builds while assembling that metadata. `scripts/ci/local-maven` removes
references to unpublished variants from the local SDK metadata on both hosts: Amper
resolves those references even for JVM qualification. The persistent macOS app
also refreshes its Maven-local dependency resolution after publication, keeping
compiled outputs while discarding the obsolete dependency graph.
`scripts/ci/shopping-project` prepares an Amper workspace over the same application
sources for the host's published platforms: JVM/Android on Linux, device/simulator
iOS on macOS. `ROCKY_SHOPPING_PROJECT` selects it for build, tests and qualification,
so resolution never requests variants absent from that host's publications.
The macOS job publishes and tests
the iOS artifacts; its host needs full Xcode selected with `xcode-select`, an
installed iOS simulator runtime and an available iPhone simulator. It installs
Java 21 for the Kotlin checks. `scripts/ci/ios-simulator` verifies a real boot before
compilation. If the installed cryptex runtime has a stale path in the runner's
CoreSimulator service, it refreshes only that user's service and retries the boot.
Gitea resolves the current Temurin 21 archive through Adoptium's major-version
endpoint, verifies its SHA-256 checksum, and installs it with `setup-java`.
This avoids the action's paginated lookup of every Temurin release, which has
timed out before lint could start.
The tier budgets apply in CI as they do locally, scaled by the runner's load;
no hosted run has measured them yet. CI retries a failing nextest test once
(`NEXTEST_RETRIES`).

The `android` job runs on pull requests and pushes. It performs the release
workflow's Android build steps to keep two caches warm: the Gradle and Kotlin
CLI toolchain files, and the Cargo registry with the native target directory of
`sdks/native`. The release workflow's Android job restores both and saves them
again, keyed by the files that decide their content, so a nightly build does not
recompile the Android runtime from nothing. The cache keys are identical in both
workflows; changing them means one cold build. On Gitea, whose runner image has
no Android SDK, both jobs also cache the Android platform and NDK.

Required status checks are a hosting setting; the workflow file does not configure
branch protection. No remotely hosted CI run has been verified by this baseline.

The `shopping` job runs `./examples/shoppinglist-app/qualify` beside the
baseline. It also runs `sdks/kotlin/gradlew -p sdks/kotlin check`, which builds the Android
runtime, and runs the exhaustive phase-2 byte-offset cancellation gate in the release
profile. This job uses an x64 runner for the Android NDK and has not run on a
hosted runner yet. Releases skip it and do not wait for its result.

A release waits for the latest push run's `check` job for that exact commit.
It proceeds as soon as that job succeeds, even while auxiliary jobs run or
fail. A missing or unfinished `check` keeps it waiting; a failed, cancelled or
skipped `check` fails the release gate. The release then calls only `build`
and `check` in the baseline workflow; `lint`, `e2e`, `shopping`, baseline
`android` and baseline `ios` do not gate it. When `check` passed for the tree,
the release's `build` and `check` skip. A job whose build artifact is missing,
for example after its one-day retention, builds what it runs itself.

Only the release workflow builds and publishes the container images, so the
Actions sidebar lists just the baseline, commit-message and release workflows.

The commit-message workflow checks every pushed and pull-request commit
([commit messages](commits.md)). The release workflow publishes nightly and on
demand to the hosting forge's packages; it is the one workflow that clones
history, which git-cliff needs to compute the version ([releasing](releasing.md)).

### Adding a CI job

A job that reuses the build is written like `check` in
[the baseline workflow](../.github/workflows/check.yml); later phases add their
jobs there. It needs `build`, pins the same runner, keeps the source step and
then uses two actions in `.github/actions`:

- `reuse-build`, given the job's marker name and the source tree, looks up the
  job's passing marker. When there is none it installs the system libraries,
  the pinned toolchain and cargo-nextest (`nextest: false` skips it), downloads
  the artifacts the job asks for (`workspace-archive`, `sdk-archive`, `peer`)
  to `$RUNNER_TEMP/build` and restores the Rust cache; `dependencies: fallback`
  restores it only when a requested artifact is missing.
- Every job that runs Cargo uses `setup-sccache` after the toolchain: it
  installs the pinned `sccache` (`scripts/ci/install-sccache`) and, on GitHub,
  keeps its cache in the Actions cache; Gitea runners cache on their own disk.
- The job's own steps run only when `steps.<id>.outputs.passed` is not `true`.
  They run the artifacts that arrived, such as
  `scripts/check --from "$RUNNER_TEMP/build"`, and build the rest.
- `record-pass` with the same marker name, last, under the same condition.

A job that cannot run the Linux artifacts, such as one on a macOS runner, asks
for none and builds what it runs from the restored cache; the system library
step does nothing outside Linux.

### Gitea

The Gitea mirror runs the same `.github/workflows`; it has no `.gitea` directory,
which would replace them. The workflows call the REST endpoints both forges serve
and never `gh`. Where they differ, steps branch on `github.server_url`:

- Linux jobs run in the runner image, which lacks rustup and the Android SDK; the
  jobs install them. Linux jobs use GitHub's labels, `ubuntu-latest` (x64) and
  `ubuntu-24.04-arm`, which the Gitea runners must carry. Jobs that need no
  particular architecture use `ubuntu-latest-any-arch` on Gitea, carried by
  runners of both architectures. Only the Android builds,
  the `linux-x86-64` JVM SDK and the amd64 image need x64; the Android NDK has no
  arm64 Linux host. The baseline `build`, `check` and `e2e` jobs exchange
  architecture-specific binaries, so they pin the faster x64 `ubuntu-latest`
  runner. `shopping` uses the same baseline peer for its Android qualification.
- The release job that produces the `darwin-aarch64` JVM SDK still uses macOS
  because it builds a macOS native library. Other release jobs use Linux.
- Images and Maven artifacts go to the instance's registries with the
  `PACKAGE_TOKEN` secret; releases are created through the Gitea API.

## Code and tests

Use small public APIs and descriptive private modules. Return typed errors for
expected failures. Keep external I/O outside database transactions and publish
only committed state. Avoid unnecessary traits, production panics and unwraps.

Inherited lints forbid unsafe code and reject `dbg!` and `todo!` placeholders.
Add targeted lint policy when a concrete recurring problem justifies it.

Tests should establish observable behavior, invariants or regressions. Prioritize
convergence, atomic rollback, observation races, deduplication and fencing as
those behaviors are implemented. Use controllable fault injection rather than
arbitrary sleeps. Empty-crate checks only establish the development baseline.

## Benchmarks and local data

All benchmark and probe code lives under `benchmarks/`. The two engine
workspaces, `engine-comparison` and `engine-scale`, are outside application
workspace checks; follow the
[instructions](../benchmarks/engine-comparison/AGENTS.md) before changing them.
`runtime-scaling` holds the runtime's scaling probes and stays a workspace
member, so `./scripts/check` keeps it compiling against the runtime API.

Keep development databases and credentials under the ignored `local/` directory.
Do not commit private keys or real `.env` files. Preserve deliberately checked-in
benchmark evidence and summaries.
