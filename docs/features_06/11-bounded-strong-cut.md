# 11. Bounded strong cut

Status: done on 2026-10-08. [Decision 0039](../decisions/0042-bounded-strong-cut.md) records the format.
Owner: `rocky-runtime` strong worker and request/result frames; `rocky-secure`
command path. Depends on nothing; lands after
[phase 1](01-remove-compatibility-code.md) on the frame. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [strong publication](../runtime/strong-publication.md),
[checkpoints](../runtime/checkpoints.md#design-rationale) and
[strong history](../runtime/strong-history.md). A strong request names its cut
by frontier, and the executor expands that frontier into every operation
identity from sequence 1 (`rocky-runtime/src/worker/strong.rs`, the `Quota`
check on the frontier sum and the `required` list). At 24 bytes per identity
against the 1 MiB frame, a domain refuses every strong command once its authors
have written about 43,690 operations in total. Pruning does not lower the sum,
so the refusal is permanent. A cut must cost what was written since the
pruning base, not the domain's lifetime.

## Ordered slices

1. Decision record: the cut is named by the checkpoint base plus the
   operations after it, or by the frontier with a digest of the covered
   operations. State what a verifier checks and what a result frame carries.
   One format; any other is refused.
2. Executor: derive the covered operations from the base; the bound counts
   only operations after it. A request whose base the executor has pruned
   past, or does not hold, is `StrongPending`, never a silent widening.
3. Verification on every replica and in the secure command path uses the same
   derivation; a replica that pruned behind the cut still verifies the result.
4. A rejected operation inside a requested cut: the count excludes
   `status='rejected'` while the expected count does not. Prove by test that
   this cannot leave a permanent `StrongPending(MissingInput)`, or fix it.
5. Guides and the measured-limits table state the new bound.

## Completion gate

- A domain with more than 100,000 lifetime operations, pruned to a recent
  checkpoint, executes and verifies a strong command.
- The bound is reached only by operations after the base, and is reported as
  `Quota` with the limit.
- A replica that rebootstrapped from the checkpoint verifies the same result.
- An unknown or pruned base is refused before any operation is loaded.
