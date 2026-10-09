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
workspace tests, including doc tests. Dependency resolution uses the committed
lockfile.
It uses [cargo-nextest](https://nexte.st) when installed, which runs every test
binary in parallel, and falls back to `cargo test`. Settings are in
`.config/nextest.toml`: tests that take over 60 seconds are reported as slow;
the coordinator Raft store probe gets all test threads because parallel tests
would distort its fsync latency measurement. CI disables fail-fast and
terminates an individual test after ten 60-second timeout periods.

Soak tests are `#[ignore]`d and named `soak_*`; `./scripts/check --slow` runs
only those. Keep a short variant of each soak test in the baseline.
Dependencies are built with `opt-level = 2` in the dev profile, since crypto
and SQLite dominate test time; workspace crates stay unoptimized.

During development, narrow verification to the package you changed:

```sh
cargo test -p rocky-runtime --locked
cargo clippy -p rocky-runtime --all-targets --locked -- -D warnings
cargo fmt --all
```

Run the shared checks before completing workspace changes. Introduce explicit
checks for supported feature combinations when platform features are added.
Do not assume `--all-features` represents a valid platform configuration.
`./scripts/check` also tests `rocky-sdk` without its default `iroh` feature,
and `./sdks/native/check` builds the ABI without it.

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

## Android

The [Android runtime](../sdks/kotlin/android-runtime/README.md) is a small Gradle build
that publishes Rocky's native libraries to Maven local; Android apps depend on the
`rocky-runtime-android` module, which checks that the published runtime is current.
It needs the Android SDK and NDK and rustup, which it provisions under `local/` when
none is on `PATH`. `./kotlin do packageAndroid -m rocky-runtime-android` then writes the
SDK's AAR and jars as a local Maven repository for Gradle consumers.
This is not part of `./scripts/check`. See [Android](runtime/android.md).

## CI

The baseline workflow's `lint` job runs `scripts/tools` and `scripts/lint` on
Linux; it needs no build and fails within minutes. The `build`, `check`, `slow`
and `shopping` jobs also run on Linux, so CI never builds on the macOS host.
On GitHub they use `ubuntu-latest`; on Gitea they use `ubuntu-latest-any-arch`.
The `build` job downloads an archive of the exact
event revision and runs `scripts/check --build`: strict workspace Clippy, then
the test binaries as nextest archives, uploaded with the custody peer binary.
Gitea uses `moonrepo/setup-rust` to install nextest and cargo-insta for the
runner architecture; GitHub uses `taiki-e/install-action` for nextest.
`check` runs `scripts/check --from` those archives: `cargo fmt --all -- --check`,
the tests and doc tests, without compiling the test binaries again. `slow`,
`shopping` and `android` also start after `build`; the first two run its
archives and peer binary.
It needs no version-control CLI or repository history, and never invokes Git.
The repository toolchain file is also the toolchain source of truth in CI.
The shared CI toolchain setup installs `libclang` for bundled SQLite's bindgen
build when the runner image does not provide it.

A job that passes records a cache marker keyed by the source tree. A later run
of the same tree, such as a redescribed or rebased commit with unchanged content
or the release of an already-checked commit, finds the marker and skips its
steps. Any change to a tracked file, including the workflow, runs it again.
When `build` finds `check` already passed for the tree it builds nothing, and
the jobs after it build what they still run themselves. The Rust dependency
cache is saved from `main` and restored on other refs.

The `slow` job runs `./scripts/check --slow` on pushes to `main`; releases do not
wait for it.

The `android` job runs only on pushes to `main`. It performs the release
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
baseline; it has not run on a hosted runner yet. Releases skip it and do not
wait for its result.

A release calls the baseline workflow. Before it starts, it waits only for the
push run's `lint`, `build` and `check` jobs for that commit; a failure in any of
them fails the release. The `slow`, `shopping` and baseline `android` jobs do
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
  arm64 Linux host.
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
