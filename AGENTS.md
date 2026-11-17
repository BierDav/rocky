# Working in Rocky DB

## Navigation

- Read [README.md](README.md) and applicable nested `AGENTS.md` files first.
- Learn behavior, ownership and phase prerequisites from source, manifests and
  tests; map the workspace with `cargo metadata --no-deps --format-version 1`.
- Search with `rg`, starting in the owning crate and following definitions and
  callers. Skip generated output and benchmark evidence.
- Documentation is AGENTS files, the root README and current `docs/features_xx`
  roadmaps, which hold phase status and measured evidence. Add no other docs.

## Design

- Leave code more intuitive than you found it. Restructure modules, types and
  crate boundaries when the current shape no longer makes sense, even when that
  touches more code than a quick fix would. Remove unnecessary code.
- Call out features that do not belong in their crate, or in Rocky at all, and
  move them to the right crate or remove them.
- Rocky's API is Rust-native and typed end to end; no stringly or JSON call
  surface in the SDK; capability contracts stay open to ORM and transport
  adapters. Networking is optional; shutdown never closes application-owned
  infrastructure.
- Protocol types stay independent of storage, transport and UI. The coordinator
  owns generic metadata only; applications own reducers and business rules; the
  runtime owns transactions, operation storage and observation.
- Commit projection, operation, outbox and revision changes atomically; publish
  and notify only after commit; keep external calls outside DB transactions.
- Use precise operations with stable identities and safe retries.
- Keep modules private unless a public boundary needs them; use traits only at
  real replacement boundaries.
- Return errors for expected failures; no production panics or casual unwraps.
  Never report success for unimplemented behavior.
- Keep one persisted/wire format and reject others: no migrations or legacy readers.

## Comments

- Rust code holds no comments of any kind: no `//` or `/* */` comments, no doc
  comments and no `#[doc = ...]` text (`scripts/lint-rust`). Code explains
  itself through names, types, guards and tests.
- Encode a reason or warning as, in order of preference: a type or guard that
  makes the mistake impossible; a test named after the rule or an assertion
  message; a clearer name for the item, parameter or constant; otherwise delete
  it. Examples are ordinary tests, not doctests.
- Packages in `scripts/comment-budgets.toml` still carry old comments. Convert
  them when you touch that code; budgets only go down.

## Version control

- Use Jujutsu (`jj`) only, never the `git` CLI. Preserve unrelated changes and
  leave edits in the current change. Push, rewrite shared history or create
  bookmarks only when asked.
- Use Conventional Commits, validated by `scripts/commitlint --jj`; push with
  `jj push`.

## Verification

- Use `#[expect(lint, reason = "...")]`; `allow` attributes are forbidden.
- Develop with `./scripts/check --unit` and `cargo test -p <package>`; finish
  with `./scripts/lint` and `./scripts/check`. `./scripts/lint --fix` formats and
  lowers comment budgets.
- Test behavior, invariants and regressions deterministically, separate from
  benchmarks: fault injection instead of sleeps, no timing assertions.
- Check supported feature combinations, not blindly `--all-features`.
- Report what passed and what checks cannot prove, such as a working storage
  adapter, desktop app or distributed protocol.
- For benchmarks, follow [their instructions](benchmarks/engine-comparison/AGENTS.md).
