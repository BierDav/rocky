# Development workflow

## Tooling

Use rustup, Jujutsu (`jj`) and ripgrep (`rg`). `rust-toolchain.toml` pins Rust
1.99.0 with rustfmt and Clippy. Invoking Cargo in the repository selects it.
Workspace crates inherit shared dependency and lint configuration.

Keep the root `Cargo.lock` committed. Update it intentionally when changing
dependencies. The engine benchmark retains its own manifest and lockfile.

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
| `./kotlin check` with a warm build: 4 min | `./kotlin check` | 95 s |
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

## Kotlin desktop integration

The checksum-pinned `./kotlin` wrapper builds the root `project.yaml` workspace.
The Amper modules configure JVM desktop targets. Android is packaged and
qualified separately; see [below](#android). The SDK's build, packaging and
qualification steps are [toolchain plugins](../sdks/kotlin/README.md#build-tooling);
`./kotlin check` runs every check and test.
Run `./examples/shoppinglist-app/check` for the actual Room/Rocky integration
and desktop compilation, `./examples/shoppinglist-app/qualify` for the
multi-process [shopping qualification](runtime/shopping-qualification.md), and `./kotlin check qualify -m inventory-qualification`
for the existing SDK contract qualification. Run `./sdks/native/check` for the
excluded UniFFI ABI crate. Run `./kotlin check packaged -m packaged-consumer` for an
artifact-only JVM consumer, or `./kotlin do packageSdk -m packaged-consumer` to
produce the standalone library/resources and example in `local/packaged-sdk`.
[Packaging](runtime/sdk-packaging.md) documents no-build-tools consumption.
Run `./kotlin test -m rocky-sqlite` and `./kotlin test -m room-qualification` for the
[SQL driver](runtime/sql-driver.md) and its pinned Room fixture. The Room gate
uses the standalone `sqlite3` CLI with JSON output for read-only inspection of
the live file; install it before running that gate.
Run `./kotlin check qualify -m schema-qualification` for hand/Room definition
equivalence and native installation, and `./kotlin test -m schema-qualification` to
regenerate the pinned Room exports and check compound-key scalar DAOs.
`./kotlin check` runs the Kotlin Toolchain checks and tests; it is not a
formatter or linter. Kotlin formatting and lint run in `./scripts/lint`.
Every build prepares SDK-owned native bindings/resources with Cargo, so it needs
the pinned Rust toolchain. These are separate from the Rust `./scripts/check` gate.
See [Room on Rocky's file](runtime/room-integration.md).

## iOS bindings

On macOS/Xcode, run `./sdks/native/check-ios` for both arm64 runtime targets
with and without iroh. `./sdks/native/qualify-ios-binding` generates and tests
the isolated no-iroh callbacks on the simulator. `./sdks/native/qualify-ios-sdk`
runs the common SDK/driver/Room tests and the endpoint, command, cancellation
and lifetime gates with iroh. [The binding guide](runtime/kotlin-native-binding.md)
records the toolchain, dependency repair, threading contract and evidence.

Phase 18 adds `./sdks/kotlin/package-ios` and
`./sdks/native/qualify-ios-package` for the independent packaged consumer, and
`./sdks/native/qualify-ios-lifecycle` for Keychain, suspension and crash probes.
The new Rust endpoint ownership/rebind tests are integration tests through
`kind(test)` in `.config/test-tiers.toml`, with the same 2 s budget. The iOS
acceptance tests run in the macOS job; they are platform integration gates,
not additions to the Rust unit tier. The lifecycle gate enforces 2 s for each
of its seven tests; the packaged consumer enforces the same budget. See
[iOS](runtime/ios.md).

## Android

The [Android runtime](../sdks/kotlin/android-runtime/README.md) is a small Gradle build
that publishes Rocky's native libraries to Maven local; Android apps depend on the
`rocky-runtime-android` module, which checks that the published runtime is current.
It needs the Android SDK and NDK and rustup, which it provisions under `local/` when
none is on `PATH`. `./kotlin do packageAndroid -m rocky-runtime-android` then writes the
SDK's AAR and jars as a local Maven repository for Gradle consumers.
This is not part of `./scripts/check`. See [Android](runtime/android.md).

## CI

The baseline workflow runs for pushes to the default branch and for the release
workflow's call, not for pull requests. Its `lint` job runs `scripts/tools` and
`scripts/lint` on Linux; it needs no build and fails within minutes. The
`build`, `check`, `e2e`
and `shopping` jobs also run on Linux. The `ios` job uses an arm64 macOS host
for the static target matrix and simulator checks.
On GitHub they use `ubuntu-latest`; on Gitea `build`, `check` and `e2e` use
`ubuntu-24.04-arm` to exchange architecture-specific nextest archives.
`shopping` uses the x64 `ubuntu-latest` runner for the Android NDK and builds
its own custody peer on Gitea; `lint` uses `ubuntu-latest-any-arch`.
The `build` job compiles the baseline workspace. It downloads an archive of
the exact event revision and runs `scripts/check --build`: strict workspace
Clippy and the test binaries as nextest archives; it then builds the custody
peer. It uploads three independent artifacts -- `nextest-workspace`,
`nextest-sdk` and `rocky-peer` -- so a failure to finalize one cannot also lose
the others, and each consumer fetches only what it runs.
Linux jobs install the pinned Rust toolchain with `dtolnay/rust-toolchain`; the
workflow names the same version as `rust-toolchain.toml`, which `rustup` still
resolves inside the checkout. The iOS job reads that file for its official
rustup toolchain and target installation. Gitea installs the runner-architecture nextest
with `scripts/ci/install-nextest`; GitHub uses `taiki-e/install-action`.
`check` downloads both test archives and runs `scripts/check --from` them:
`cargo fmt --all -- --check` and the unit and integration tiers, without
compiling. `e2e` downloads both for the end-to-end tier. GitHub `shopping`
downloads only the x64 `rocky-peer`, while on Gitea it builds its own; `android`
needs no Rust artifact and keeps its own Gradle and native caches. The downloads
are best-effort: Gitea's artifact service sometimes fails to finalize an
artifact, and a job that finds one missing rebuilds the test binaries from the
dependency cache (`check` runs `scripts/check --no-clippy` so it does not repeat
the Clippy the `build` job already ran). `check` and `e2e` compile nothing
otherwise, so they restore the Rust cache only then.
It needs no version-control CLI or repository history, and never invokes Git.
`scripts/ci/install-system-deps` installs the libraries a bare runner image
lacks: `libclang` for bundled SQLite's bindgen build, and the GTK/WebKitGTK,
X11, `libxdo` and OpenSSL development packages the Dioxus desktop example and
the websocket transport link against.

A job that passes records a cache marker keyed by the source tree. A later run
of the same tree, such as a redescribed or rebased commit with unchanged content
or the release of an already-checked commit, finds the marker and skips its
steps. Any change to a tracked file, including the workflow, runs it again.
When `build` finds `check` already passed for the tree it builds nothing, and
the jobs after it build what they still run themselves. The Rust cache keeps
both dependencies and workspace crates; it is saved from `main` and restored on
other refs.

The `e2e` job runs `./scripts/check --e2e` on pushes to `main`; releases do not
wait for it.
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
baseline. It also builds the Android runtime prerequisite for `./kotlin check`
and runs the exhaustive phase-2 byte-offset cancellation gate in the release
profile. This job uses an x64 runner for the Android NDK and has not run on a
hosted runner yet. Releases skip it and do not wait for its result.

A release calls the baseline workflow. Before it starts, it waits only for the
push run's `lint`, `build` and `check` jobs for that commit; a failure in any of
them fails the release. The `e2e`, `shopping` and baseline `android` jobs do
not gate it. When `check` passed for the tree, the release's `build` and `check`
skip. A job whose build artifact is missing, for example after its one-day
retention, builds what it runs itself; a release of an unchecked tree runs the
build and checks.

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
  architecture-specific binaries, so they pin `ubuntu-24.04-arm` rather than the
  mixed `-any-arch` label. `shopping` uses x64 for its Android prerequisite and
  builds its peer there instead of using the arm64 artifact.
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
