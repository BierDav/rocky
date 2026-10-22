# Working in Rocky DB

## Start here

- Read [README.md](README.md) for a short project overview.
- Use source code, manifests, module declarations and tests to establish current
  behavior and choose the owning crate. Verify phase prerequisites against code.
- Follow applicable nested `AGENTS.md` files before editing a subtree.

## Version control

- Use Jujutsu (`jj`) exclusively for all version-control operations.
- Do not invoke the `git` CLI, including for status, diff, history or commits.
- Use `jj status`, `jj diff`, `jj log` and `jj file list` to inspect work.
- Git-backed remote transport is accessed through `jj git fetch` / `jj git push`.
- Preserve unrelated changes. Do not abandon or restore someone else's work.
- Describe changes with Conventional Commits; release versions
  are computed from them. Check with `scripts/commitlint --jj`; push with
  `jj push` (`scripts/jj-push`), which checks first.
- Do not push, rewrite shared history or create bookmarks unless the task calls
  for it. Leave completed edits in the current change unless asked otherwise.

## Find code efficiently

- Search with `rg --files` and `rg -n`, scoped to the owning crate first.
- Batch independent searches and reads; follow dependencies sequentially.
- Read matching definitions and callers, then expand to adjacent crates.
- Use `cargo metadata --no-deps --format-version 1` for the actual workspace map.
- Keep navigation at crate/responsibility level; never maintain an inventory
  describing every file. Manifests and source modules supply the detailed map.
- Exclude generated output and benchmark evidence from ordinary code searches
  by selecting the relevant source directory, rather than searching everything.

## Code structure and comments

- As a side task during implementation, remove unnecessary code and simplify
  existing code wherever practical. Keep the codebase lean, preserve required
  behavior and keep cleanup relevant to the work at hand.
- Keep the code easy to navigate with focused modules, clear names and explicit
  ownership. Follow definitions and callers instead of maintaining file inventories.
- Keep only AGENTS instructions, the simple root README and `docs/features_10`
  as project documentation. Do not recreate removed guides or architecture docs.
- Keep comments and API docs short, focused on intent, invariants or surprising
  constraints. Express behavior through code and meaningful tests.
- Record phase status and measured evidence within `docs/features_10`.

## Implementation boundaries

- Keep capability contracts language-neutral and extensible to qualified language,
  ORM and transport adapters. Consumers must be able to define schemas/handlers
  and get started with packaged artifacts without Rust code or a Rust project.
- Keep network ownership optional: never close application-owned infrastructure
  as an incidental database/session shutdown.
- Keep shared protocol types independent of storage, transport and UI.
- The coordinator handles generic metadata, never application payloads or rules.
- Applications define deterministic reducers and business transitions.
- The runtime owns database transactions, operation storage and observation.
- Commit related projection, operation, outbox and revision changes atomically.
- Publish outgoing data and observer notifications only after local commit.
- Keep network, coordinator and other external calls outside DB transactions.
- Use precise operations with stable identities; retries must remain safe.
- Keep internal modules private unless a supported public boundary requires them.
- Use traits at real replacement boundaries; avoid speculative abstraction.
- Return errors for expected failures. Avoid production panics and casual unwraps.
- Unsafe code is forbidden by the inherited workspace lint policy.
- Do not add placeholder implementations that report success for missing behavior.
- Keep no backward compatibility. Each persisted and wire format has one current
  version; do not add migrations, legacy readers or dual encodings. Refuse any
  other version instead of interpreting it.

## Verification

- During development, use `./scripts/check --unit`, `cargo test -p <package>`
  and targeted checks. Keep each test within its configured tier's budget.
- Before finishing workspace changes, run `./scripts/lint` and `./scripts/check`
  from any directory. `./scripts/lint --fix` applies formatting.
- Test meaningful behavior, invariants and regressions, not trivial duplication.
- Prefer deterministic fault injection over tests dependent on arbitrary sleeps.
- Check explicitly supported feature combinations; do not blindly use
  `--all-features` for incompatible platform configurations.
- Report what passed and any limitations; baseline checks do not prove a working
  storage adapter, desktop app or distributed protocol.
- Benchmark changes use [their own instructions](benchmarks/engine-comparison/AGENTS.md).
