# 10. Qualification

Status: not started.
Owner: the shopping example, Android qualification and the audit. Depends on
every earlier phase. Wave 5.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the audit](../runtime/shopping-qualification.md), its
[acceptance scenarios](../runtime/shopping-qualification.md#acceptance-scenarios)
and the acceptance lists of both scopes. The earlier qualification stopped because the shopping schema did not install.
This phase runs its outstanding scenarios and the new ones, and records
evidence.

## Ordered slices

1. Phase 16's scenarios 1 to 5 and 7 on the group-based schema; scenario 3 as
   resolved in [the scope](scope-partitioned-constraints.md#resolved-questions).
   Scenario 6 (market) is outside this roadmap; the audit keeps it listed as
   outstanding with its own blocking contract.
2. Acceptance 1 to 7 of [partitioned constraints](scope-partitioned-constraints.md#acceptance)
   in CI, multi-process where noted.
3. [Move acceptance](scope-moving-rows.md#acceptance) in CI and on two phones.
4. Sharing and revocation through the group secret on two phones with the
   always-on custody peer.
5. Refusal regressions for every shape listed under
   [not requested](scope-partitioned-constraints.md#not-requested) and for
   references between groups.
6. Update the audit, the getting-started guide .

## Completion gate

- Every scenario above has a reproducible command or device procedure and
  recorded evidence.
- The [release gate](README.md#release-gate) is shown with no Rust source in
  the application.
- The audit lists no outstanding scenario, or names each remaining one with
  its blocking contract.
- `./scripts/check` and the Kotlin, native and packaged checks pass.
