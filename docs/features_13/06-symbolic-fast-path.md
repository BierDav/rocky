# 6. Symbolic fast path

Status: planned. Owner: new crate `crates/rocky-symbolic`; its caller in
`rocky-secure` replay. Depends on [phase 3](03-intent-shapes.md) and
[phase 5](05-replayed-constraints.md). Effort: two weeks. Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

Avoid rewinds where they are provably unnecessary. A late arrival that
commutes with every transaction it would cross is applied directly at
the current state. The result must equal the rewind's result exactly,
including drops.

## Settled

- **Crate.** `rocky-symbolic` depends only on `rocky-core`. No SQLite,
  no I/O, no async. It holds the intent algebra, the pairwise
  commutativity proof and symbolic composition. It is private to the
  workspace; `rocky-secure` is its only caller. There is no engine trait.
- **Algebra.** Exactly the operations phase 3 selected from: keyed
  assign, numeric add, string append, keyed delete and fresh-key insert,
  each on one row of one partition. A statement outside the set is
  opaque, and every pair involving it requires a rewind.
- **Constraints decide commutativity too.** Two transactions commute
  only if both state and outcomes are order-independent. A pair touching
  a column covered by a unique index, foreign key, `CHECK` or assertion
  is treated as non-commuting unless the proof covers that constraint.
  Initially only `NOT NULL` and checks that read just the assigned column
  with a constant bound are covered. Numeric adds against such a bound
  never commute: whether they fail depends on order.
- **Proofs are over all inputs.** A proof says `A(B(x)) == B(A(x))` for
  every admissible state, not for the current rows. Numeric bounds follow
  the prototype's finite contract, so no prefix can overflow `i64`.
- **Merge rule.** The crate is merged only if both hold:
  1. Exhaustive equivalence: for every fixture and schedule from phase 4
     slice 7 and phase 5 slice 6, fast-path state and drop map equal the
     rewind's.
  2. A paired advantage: late-arrival apply in the phase 3 mix is at
     least 20% cheaper at p95 than rewind on desktop, with statement
     counts ≤ in-order receive + 5.

  If either fails, record the evidence and delete the crate. Phase 8
  proceeds without it.

## Ordered slices

1. Crate skeleton with the algebra types lowered from phase 4 intent
   frames; refusal to lower anything outside the set.
2. Pairwise commutativity with constraint awareness; brute-force tests
   against an independent ordered interpreter over a matrix of bound
   operations, as in the prototype.
3. Caller in `rocky-secure`: if every crossed pair commutes, apply
   directly; otherwise rewind. A test-only switch forces rewind for the
   equivalence runs.
4. Equivalence runs over all phase 4 and phase 5 schedules.
5. Paired measurement of late-arrival cost, fast path against forced
   rewind, release build.

## Completion gate

The merge rule above, with its evidence. `./scripts/check` passes.

## Not in scope

Symbolic composition of whole tips, predicate updates, SMT solving and
author-side compaction (the prototype found its payload savings don't
reach durable operation counts).
