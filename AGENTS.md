# Working in Rocky DB

## Start here

- Read [README.md](README.md) for project status and entry points.
- Read [the current roadmap](docs/features_06/README.md) and its linked architecture
  before selecting new implementation work. Older phase plans are historical or
  deferred integration references, not competing next-work instructions.
- Read [docs/modules.md](docs/modules.md) to choose the owning crate.
- Read [docs/development.md](docs/development.md) for tooling and checks.
- Follow applicable nested `AGENTS.md` files before editing a subtree.

## Version control

- Use Jujutsu (`jj`) exclusively for all version-control operations.
- Do not invoke the `git` CLI, including for status, diff, history or commits.
- Use `jj status`, `jj diff`, `jj log` and `jj file list` to inspect work.
- Git-backed remote transport is accessed through `jj git fetch` / `jj git push`.
- Preserve unrelated changes. Do not abandon or restore someone else's work.
- Describe changes with [Conventional Commits](docs/commits.md); release versions
  are computed from them. Check with `scripts/commitlint --jj`; push with
  `jj push` (`scripts/jj-push`), which checks first. See [linting](docs/linting.md).
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

## Documentation and comments

- Prefer documentation under `docs/` over long explanations embedded in code.
- Keep code comments short and focused on intent, invariants or surprising
  constraints. Avoid essays and narration of obvious code.
- Keep API documentation concise; move extended examples and design rationale
  to a focused document and link it where useful.
- Around 200 lines, consider splitting a document by reader task or topic.
  This is a review signal, not a hard limit or a reason for arbitrary fragments.
- Link related documents instead of duplicating explanations or instructions.
- Update the relevant documentation when behavior or module ownership changes.
- Follow [docs/documentation.md](docs/documentation.md) for writing and splitting.

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

- During development, use `cargo test -p <package>` and targeted checks.
- Before finishing workspace changes, run `./scripts/lint` and `./scripts/check`
  from any directory. `./scripts/lint --fix` applies formatting.
- Test meaningful behavior, invariants and regressions, not trivial duplication.
- Prefer deterministic fault injection over tests dependent on arbitrary sleeps.
- Check explicitly supported feature combinations; do not blindly use
  `--all-features` for incompatible platform configurations.
- Report what passed and any limitations; baseline checks do not prove a working
  storage adapter, desktop app or distributed protocol.
- Benchmark changes use [their own instructions](benchmarks/engine-comparison/AGENTS.md).
