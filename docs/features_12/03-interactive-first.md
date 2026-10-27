# 3. Interactive first

Status: planned. Owner: `rocky-secure` (the replica lock wrapper from
phase 1 and the step loops from phase 2). Depends on
[phase 2](02-bounded-holds.md). Effort: one week. Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

With bounded steps, a local write still waits behind every background
waiter queued before it: group leaders, settle, maintenance, link tasks.
After this phase it waits for at most the one step already running.

## Settled

- **A two-class gate in the phase 1 wrapper.** The wrapper keeps a count
  of registered interactive waiters.
  - An interactive acquisition increments the count before it waits and
    decrements it once it acquires the lock, or if it is cancelled (use a
    drop guard).
  - A background acquisition that finds a registered interactive waiter
    waits until the count is zero before it queues on the mutex.
  - Interactive acquisitions among themselves stay FIFO.
- **Yield at step boundaries.** Every bounded loop from phase 2 checks the
  count between steps. If an interactive waiter is registered, the loop
  releases the replica and re-acquires as background. Nothing yields
  mid-step or mid-transaction.
- **Starvation guard.** A background loop that has yielded 8 times in a
  row runs its next step without checking, then resumes yielding. The
  constant is named and documented. Background work therefore makes
  progress under continuous interactive load, at worst one step per 8
  interactive transactions.
- **Determinism.** The gate uses no clocks, so seeded simulation still
  reproduces exactly. Tests drive interleavings with the existing
  fault-injection and pause points, not sleeps.
- **Transitions wait for a quiet gate.** A [phase 7](07-transitions.md)
  transition is one unbounded step. Before it starts, it waits until no
  interactive waiter is registered, then acquires as background. Once
  started, it runs to completion, as today. The starvation guard applies
  to its start in the same way.
- **Dependent fan-out waits for a quiet gate.** An oversized
  [dependent fan-out step](02-bounded-holds.md#settled) starts only when no
  interactive write is waiting and then commits all induced child changes
  atomically. Handle its start like a transition; the starvation guard must
  not bypass this explicit no-waiter requirement. Report overlapping latency
  samples separately. The exception expires in features_13 when constraints
  move into replay.
- **The flush is not a holder.** Flushes already run without the replica
  and are unaffected.

## Ordered slices

1. Interactive waiter count and background deferral in the wrapper.
   Tests:
   - A background acquisition queued before an interactive one acquires
     after it.
   - A cancelled interactive waiter leaves the count at zero.
   - Interactive acquisitions keep FIFO order.
2. Step-boundary yields in every phase 2 loop and in the group leader's
   drain loop. Test: with an interactive waiter registered mid-loop, the
   loop releases after the current step, and the waiter acquires before
   the next step.
3. Starvation guard. Test: continuous interactive acquisitions still let
   a background loop complete, with the expected step count.
4. Transition and dependent fan-out start gates. Test each: work requested
   while an interactive waiter is registered starts only after that waiter
   acquires and releases. A fan-out start never bypasses a waiting interactive
   write through the starvation guard.
5. Simulation: add the interactive probe to the fixed seed set. Every seed
   converges, and blockers decrease as before.
6. Rerun L0–L4 on desktop, and on phone where available, plus the desktop
   sync non-regression rows.

## Completion gate

- In L1–L4, every interactive acquisition waits for at most one
  background step. A transition already running counts as that step.
  Assert this deterministically with a counter in test support, and
  confirm it in the measured runs.
- Desktop interactive write in L1–L4 meets p95 ≤ 16 ms, p99 ≤ 50 ms and
  max ≤ 100 ms, excluding samples that overlap a running transition.
  Those samples, and samples overlapping oversized dependent fan-out, are
  counted and reported separately. Phone results where
  measured are recorded against 33 / 100 / 250 ms.
- The desktop sync non-regression rows hold. In particular, L1 member pull
  time with the probe running is within 110% of the paired baseline
  without the probe.
- `./scripts/check` passes, including the simulation tier.

## Not in scope

Shortening application transactions, preempting a running step or
transition, and priorities among background holders.
