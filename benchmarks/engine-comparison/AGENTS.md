# Engine benchmark work

The root [AGENTS.md](../../AGENTS.md) also applies, including Jujutsu-only version
control, short code comments and the preference for focused documentation.

## Scope

- This is an independent Cargo workspace, excluded from application checks.
- Read [README.md](README.md) for reproduction commands before running benchmarks.
- Read [CRITERIA.md](CRITERIA.md) and [DECISION.md](DECISION.md) before changing
  comparisons or interpreting measurements.
- Search `src/` or the relevant analysis script, not all generated evidence.
- Keep historical evidence and curated summaries intact unless the task calls
  for updating them. Label newly measured results with their environment.

## Verification

- For Rust changes, build with `cargo build --release --locked` here and run
  `./target/release/rocky-engine-bench check`.
- Choose additional reproduction runs based on the behavior being changed.
  Do not start the complete expensive matrix for unrelated baseline edits.
- Do not extrapolate benchmark correctness into framework integration guarantees.
- Consider splitting documentation around 200 lines; preserve historical context
  when deciding whether an existing evidence document should remain intact.
