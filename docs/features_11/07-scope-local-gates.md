# 7. Scope-local gates

Status: implemented; shopping-app profile not verified. See the
[evidence](07-scope-local-gates-evidence.md). Owner: `rocky-secure`
readiness, partition authorization and SQL sessions. Depends on [phase 1](01-see-every-stall.md),
[phase 2](02-deterministic-simulation.md) and
[phase 4](04-recovery-that-drives-itself.md). Effort: two weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read the [review](00-reliability-review.md), finding 5. One scope that
needs a rebootstrap, has an unsealed import or lost a compaction refuses
writes and reads in every scope of its domain. The checks repeat per
session scope and per partition scope against every domain scope, and one
statement is parsed five or six times.

After this phase a gate refuses only what depends on the blocked state,
each derivation runs once per domain per revision, and a statement is
parsed once.

## Settled

- **Inventory first.** The first slice lists every domain-wide gate with
  the cross-scope dependency that justifies it (for example the identity
  ledger or an author's chain across scopes) or marks it scope-local. The
  inventory is reviewed before code changes and kept in the evidence.
- **Scope-local by default.** Writes and reads in a scope are refused only
  for that scope's blockers and for domain blockers the inventory keeps.
  Partitioned tables report incomplete only for partitions whose scope is
  blocked.
- **Derive once.** The blocked-scope set per domain is cached under the tip,
  the authority version and the revision of the tables it reads, and
  invalidated by their writes. `begin` checks per domain, not per scope.
- **Parse once.** A bounded cache keyed by SQL text holds the parsed
  statement, its relations and view expansion; per-table rules are cached
  under authority and partition versions. Held partition values bind as
  parameters instead of literals, so statement text is stable.
- **Proof by simulation.** The phase 2 sweep runs with assertions that a
  write accepted under a scope-local gate never produces a state the audit
  refuses, on any node, after healing.

## Ordered slices

1. The gate inventory and its review.
2. Cached blocked-scope sets and per-domain checks.
3. Scope-local refusals for gates the inventory releases.
4. Parse cache, rule cache and bound partition values.
5. Simulation assertions and evidence.

## Completion gate

- In a domain where one scope awaits a seal rebootstrap, a write and a read
  in another scope succeed, and a write in the blocked scope is refused
  with its blocker.
- Counting hooks show one readiness derivation per domain per revision for
  a transaction touching 64 scopes, and one parse per distinct statement.
- The audit oracle and the e2e simulation sweep pass with scope-local
  gates; no seed produces a refused state.
- Control SQL in the shopping app on a household with ten partitions spends
  no sampled time in readiness checks for unrelated partitions.
