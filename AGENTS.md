# Working in Rocky DB

## Navigation and cleanup

- Read [README.md](README.md) and applicable nested `AGENTS.md` files before editing.
- Establish behavior, crate ownership and phase prerequisites from source,
  manifests, modules and tests. Use `cargo metadata --no-deps --format-version 1`
  for the workspace map; never maintain per-file inventories.
- Search with `rg --files` / `rg -n`, starting in the owning crate's source and
  following definitions and callers into adjacent crates. Exclude generated
  output and benchmark evidence. Batch independent reads; follow dependencies
  sequentially.
- Simplify relevant code and remove unnecessary code while preserving behavior.
  Use focused modules, clear names and explicit ownership.
- Keep project documentation to AGENTS instructions, the simple root README and
  current `docs/features_xx` roadmaps. Do not recreate removed guides or architecture
  docs. Record phase status and measured evidence in the owning roadmap.
- Keep comments and API docs brief: intent, invariants and surprising constraints.
  Express behavior through code and meaningful tests.

## Version control

- Use Jujutsu (`jj`) exclusively; never invoke the `git` CLI. Inspect with
  `jj status`, `jj diff`, `jj log` and `jj file list`; fetch via `jj git fetch`.
- Preserve unrelated changes; never abandon or restore someone else's work.
  Leave edits in the current change. Push, rewrite shared history or create
  bookmarks only when requested.
- Use Conventional Commits (they determine release versions); validate with
  `scripts/commitlint --jj`. Push through `jj push` (`scripts/jj-push`), which
  checks first; Git remote transport uses `jj git push`.

## Implementation boundaries

- Keep capability contracts language-neutral and extensible to qualified language,
  ORM and transport adapters. Consumers must be able to define schemas/handlers
  and start with packaged artifacts without Rust code or a Rust project.
- Keep networking optional; database/session shutdown must never close
  application-owned infrastructure.
- Keep shared protocol types independent of storage, transport and UI. The
  coordinator owns generic metadata, never application payloads or rules;
  applications own deterministic reducers and business transitions; the runtime
  owns transactions, operation storage and observation.
- Commit related projection, operation, outbox and revision changes atomically.
  Publish outgoing data and observer notifications only after local commit.
  Keep network, coordinator and other external calls outside DB transactions.
- Use precise operations with stable identities and safe retries.
- Keep internal modules private unless a supported public boundary requires them.
  Use traits only at real replacement boundaries; avoid speculative abstraction.
- Return errors for expected failures; avoid production panics and casual unwraps.
  Unsafe code is forbidden by workspace lints. Never report success for
  unimplemented behavior.
- Keep one current persisted/wire format; reject other versions. No backward
  compatibility, migrations, legacy readers or dual encodings.

## Verification

- During development, use `./scripts/check --unit`, `cargo test -p <package>` and
  targeted checks. Before finishing workspace changes, run `./scripts/lint` and
  `./scripts/check` from any directory; `./scripts/lint --fix` applies formatting.
- Separate correctness tests from benchmarks. Test meaningful behavior, invariants
  and regressions without speed dependencies, latency budgets, large datasets or
  trivial duplication. Timing is diagnostic; timeouts only stop hangs. Prefer
  deterministic fault injection over arbitrary sleeps.
- Check supported feature combinations; do not blindly use `--all-features` with
  incompatible platform configurations.
- Report passed checks and limitations: baseline checks do not prove a working
  storage adapter, desktop app or distributed protocol.
- For benchmarks, follow [their instructions](benchmarks/engine-comparison/AGENTS.md).
