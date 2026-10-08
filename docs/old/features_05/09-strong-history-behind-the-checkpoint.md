# 9. Strong history behind the checkpoint

Status: complete; all ordered slices and shared qualification pass (2026-10-08).
Owner: `rocky-secure` commands, checkpoints, recovery
transfer and maintenance; `rocky-sdk` rebootstrap. Depends on
[phase 3](03-bootstrapping-from-checkpoints.md); its checkpoint statement
change lands after [phase 7](07-row-identity.md)'s
([hazards](README.md#shared-format-hazards)). Independent of
[phase 15](15-horizon.md). Wave 1. This is slice C of section 6.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [slice C](agreed-scope.md#slice-c-strong-history-behind-the-checkpoint-new-format)
and [retained history](agreed-scope.md#retained-history) of the scope,
[decision 0010](../decisions/0010-checkpoints-and-pruning.md#strong-commands),
[encrypted checkpoints](../runtime/encrypted-checkpoints.md#retries) and the
[device limits](../runtime/end-to-end-evidence.md#device-limits). Inspect
`command_budget` in `crates/rocky-secure/src/command.rs`, the
`secure_result_ack` table and the once-per-open audit.

A device stops accepting strong commands for good at 4,096 records or 32 MiB;
an executor stops after about 940 commands. Port decision 0010 to the
encrypted path: a strong base in the checkpoint, one horizon per domain from
acknowledgments, pruning in maintenance, readers starting at the base. Any
brief that puts document numbers, stock or postings behind commands depends
on this.

## Ordered slices

1. Decision record: the strong base in `StrongSummary`, digests covering only
   records above the base, confirmed command rows and the latest result in the
   rebootstrap export, versioning and refusal.
2. Horizon per domain: the lowest sequence every `required` peer not in
   `behind` has applied, less `window`, from `secure_result_ack`. Nothing new
   on the wire.
3. Pruning in the existing maintenance step: results below the horizon whose
   jobs are observed, preparations whose result is stored and stale
   preparations of settled requests, the invoker's own commands once settled.
   The latest result and any result with an unobserved job stay. A retried
   handle whose command was pruned returns `Error::Pruned`.
4. Readers start at the base: the open audit and full reconcile, the behind
   signal from `missing_results`, rebootstrap through phase 3, recovery
   packages carrying the base and the retained suffix.
5. Measure reopen latency against the 2026-10-07 numbers (177 ms at 100
   retained results, 503 ms at 400) and record the bounded result.

## Completion gate

- An executor and a reader pass 4,096 confirmed commands without hitting the
  budget; storage plateaus with `window`.
- A peer behind the strong horizon rebootstraps and confirms the next command;
  a writer enrolled after a revocation no longer reads `Incomplete`.
- A pruned command cannot be retried under a new identity; recovery packages
  verify from the base.
- First command after reopen is bounded by `window`, recorded in
  [end-to-end evidence](../runtime/end-to-end-evidence.md).

## Implementation and qualification

[Decision 0032](../decisions/0032-encrypted-strong-history.md) records the strong
base, format refusal and recovery suffix. The [runtime guide](../runtime/strong-history.md)
explains domain acknowledgment horizons, maintenance, safe retries and bootstrap.
The [reopen probe](../runtime/end-to-end-evidence.md#strong-history-qualification-2026-10-08)
records the release comparison with 2026-10-07. Eight secure regressions, the
SDK catch-up regression and the release reopen probe exercise the slices.

All four behavior bullets pass:

- Executor and reader confirm 4,100 commands with a 16-result window, no
  preparations and at most 16 invocations. The locked run's result-payload
  baseline at 100 commands is 220,903 bytes; every later hundred stays within
  10%. The 4,096-record/32 MiB command budget is unchanged. Permanent retry
  markers and retained observations are separate storage costs.
- A peer below the base bootstraps and takes over to confirm command 4,101.
  Current-policy/current-key bootstrap also completes both mergeable and
  confirmed rows for a writer enrolled after revocation.
- Recovery verifies from the base and suffix, preserves `Error::Pruned` under
  the recovered identity, and refuses a missing predecessor or corrupt base.
  Legacy unpruned checkpoint files upgrade; already-pruned legacy files refuse.
- With window 16, first commands after reopen take 34.8 ms at prefix 100 and
  46.8 ms at 400, against the previous 177 ms and 503 ms. Open and first sync
  are measured separately in the runtime evidence.

Locked package runs: `rocky-secure` passes 125 tests, five ignored, in 644.15 s;
`rocky-sdk` passes 112 tests, four ignored, in 118.53 s.

The unfiltered `./scripts/check` passes on the final implementation, 986.22 s
in total:

| Family | Result | Wall time |
| --- | --- | --- |
| `cargo fmt --all -- --check` | Pass | 0.716 s |
| Workspace Clippy, all targets, locked, warnings denied | Pass | 15.277 s |
| Workspace tests, locked | 545 passed, 10 ignored | 892.818 s |
| SDK tests without default features, locked | 108 passed, 4 ignored | 77.301 s |

Timings include build time and concurrent package qualification; the reopen
probe runs separately. The shared gate includes the owning crates, native SDK,
coordinator and transport families. This does not repeat the packaged
three-voter/WAN soak.

Conservative implementation choices are recorded in decision 0032: unknown
preparations remain bound even if an obsolete attempt used an older sequence;
a compact strong handle marker survives identity recovery independently of the
mutation handle window. The latest invocation stays until a successor so a
confirmed ticket can still return its outcome. Empty strong maintenance leaves
the mergeable pruning fault boundary intact; its existing rollback test passes.
