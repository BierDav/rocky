# Staging atomicity corrections (round40)

Status: the round41 HIGH source review accepts the bounded round40 F1/F2
corrections. Their original execution evidence follows. Whole-core acceptance
and final-source qualification remain pending.
This record does not accept phases 14/17/19 or start phase 20.

## Withdrawal

`withdraw_staged_schema` reads the durable switch intent and validates the
installed source application before changing the graph or workspace tip.
An interrupted switch's graph reload belongs to the withdrawal transaction.
Pre-commit errors restore the graph, tip, followed bookmark, selected author, registered
line nodes and shadow context. Successful publication remains after commit.

The original own-publication fixture still stages v3→v4, installs the target
schema and interrupts at `PlanStage::Switch`. It additionally checks wrong-app
refusal and a deterministic plan-deletion failure against the complete
SQL snapshot, graph, revision and author/workspace context. It then withdraws
validly on the same instance and retains its original audit/reopen assertions.

## Final staging validation

One private `Inner` helper checks the followed open source, bookmark, absence
of another private publication, candidate advancement, source minimum and exact
signed frontier. Preflight and final admission use it. Final admission holds
the same replica guard through validation, graph admission and parent freeze;
its frontier helper never reacquires the mutex. Signed bytes remain unchanged.
An intervening source write refuses the candidate before any mutation and
requires a newly prepared and signed candidate.

A test-only one-shot pause after preflight permits a different task to commit
an ordinary source SQL write. Refusal must preserve the post-write SQL, graph,
revision and author/workspace context. A second publisher holding exactly the
original frontier runs the identical signed stage and normal parent-view SQL
migration; the late original remains held on the first replica and contributes
no copied migration row; both replicas contain exactly the seed and its copy.
Competing private stages also recheck final admission.

## Execution boundary

The final focused selection passes 51/51 in 5.458 s (slowest 0.525 s), plus
7/7 retained claim/replay/proof/kept controls in 0.587 s. The concurrent-write
case takes 0.157 s; source-move and private-stage races take 0.044/0.043 s.
The SDK two-step migration without default features passes in 1.063 s; the
real private-service migration passes in its proper e2e profile in 4.569 s.
Commands and logs are in `/tmp/rocky-round40-commands.jsonl`.
The unchanged 2 s integration, 300 s integration suite, 600 s individual e2e
and 900 s combined e2e budgets remain; these subsets do not qualify the suites.

A v3→v4 removal variant reached a separate follow failure after its publisher's
migration succeeded: `items row holds a foreign field`. The actual failing
log and complete fixture are preserved for root's next HIGH review in
`/tmp/rocky-round40-new-observation.md`. It is not counted as a pass or repaired
by changing source-field validation. A v2→v3 variant with a migration DELETE
also refused at publication commit for duplicate row/mask validation; its full
log and fixture are preserved in the same observation record. The normal
migration control uses the existing v2→v3 pair with INSERT SELECT only; the
v3→v4 withdrawal fixture remains unchanged. Neither separate failure was waived. The accepted round41 corrections and
restored full failing variants are recorded in
[held-source and delete evidence](17-held-source-delete-evidence.md).

Full workspace checks, the actual combined e2e pair and supported platform
qualification remain root's responsibility after fresh source review. Earlier
matrix timing failures remain in the [performance history](17-schema-changes-through-seals-performance.md).
