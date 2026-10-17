# 0074: Owner-signed recovery cuts

Status: superseded by [0075](0075-generations-as-sealed-forks.md#tips-seals-and-bookmarks)
in [phase 19](../features_09/19-recovery-as-a-bookmark-move.md): one owner
`continue` seal replaces the cut, its chain, fencing and checkpoint taint.
Date: 2026-10-15.
Completes the cut half of [0073](0073-stuck-recovery-and-strong-commands.md)
for [phase 10](../features_09/10-stuck-recovery.md).

## Context

Collection rebuilds what any device holds. Past that, the domain owner abandons
committed entries nobody holds. Two contracts were missing: a member cannot tell
an abandoned frame from new history at a reused position, because
`wire::Membership.entry` names only a position; and a checkpoint can contain
abandoned effects whose frames were pruned, with no binding that reveals it.

## Decision

### Positions are never reused

The coordinator never rewinds. A cut is itself a log entry at `a+1`, where `a`
is the commit index it replaces; its entry hash is the cut digest. Positions
`c+1..=a` stay abandoned forever and new entries start at `a+2`. The fence is a
signed position range, so frames and bundle membership keep their format. Phase
9 aborts already reuse a position, but aborted frames never leave a node.

### The signed record

`SignedCut`, signed as `rocky.coordinated-cut.v1`, commits to:

| Field | Meaning |
| --- | --- |
| `log`, `sequence`, `previous` | Domain, this log's cut count from 1, digest of the prior cut |
| `verified: (k, r_k)` | Last position the proposer verified against the window or genesis |
| `retained: (c, h_c)` | Canonical prefix end; `k ≤ c ≤ a` |
| `replaced: (a, r_a, epoch)` | The exact coordinator head the cut replaces |
| `issued`, signer, credentials | Owner authority; see open decisions |

`h_c` chains the entry hashes `k+1..=c`; each entry hash commits to its position
and bundle frame IDs, so the record commits to every retained bundle. Entries
past the window are endorsed by this chain alone. `c = a` abandons nothing and
only endorses a gap beyond the window; it requires `h_c = r_a`. `c` must not
precede the previous cut's entry, so a cut never abandons a cut.

### Coordinator

`Action::Cut { record }` requires the domain's control authority, no pending
entry, `a`, `r_a` and epoch equal to the head, `previous` and `sequence`
following the retained cut, and `c` within bounds. It then commits `a+1` with
`running = chain(r_a, a+1, digest)`, pushes the digest into the window, clears
the in-sync set and increments the epoch. The domain retains its latest record
(at most 2 KiB) and exposes it in `LogHead`, so no device must survive to explain
the window. A retry of the retained digest returns the same `Result::Cut`; any
other stale record returns `StaleHead`. `DomainLog` gains `cut`.

### Devices fence by range

Every device stores verified cuts per log in order; a gap waits for the missing
predecessor. A frame whose signed entry `p` lies in a held range `c_i < p ≤ a_i`
is consumed without effects: stored as `abandoned`, never projected, still
continuing its author log, like a definitely absent edit. A frame naming a cut
position is refused. Senders put the cut records a receiver lacks before the
frames of that log; requests state held cut counts. By induction, every holder
of a post-cut frame holds the cut. Keyless custody cannot read positions; it
relays cuts with frames and receivers fence. Trusted nodes take cuts only from
the coordinator head; members take them from peers.

### Checkpoints bind coordinated history

A checkpoint statement gains `coordinated: { cuts, cut, highest }`: the number
and digest of applied cuts of its domain's log and the highest entry position
among covered frames. It is clean for cut `i` when `cuts ≥ i` with a matching
digest, or `highest ≤ c_i`; otherwise it is tainted. A device never seals over
a tainted base. Verification and import refuse tainted checkpoints, and a binding
naming an unknown cut waits for it. A frontier cannot replace this binding: the
owner does not know the abandoned frames' authors.

Owner-named bases were rejected: one per partition is unbounded, and the owner
would need every partition key. The binding extends the trust already given to
checkpoint signers.

### Rollback

`secure_cut_work(log, sequence, step, cursor)` drives bounded steps. Each step's
effects and its cursor commit in one transaction; notifications follow commit.

1. **Admit.** Verify the record and chain, store it with the work row; flush.
   Fencing applies from this commit.
2. **Classify.** In pages of 256, mark held frames in the range `abandoned`,
   delete their `secure_entries` rows, and make their mutation handles report
   `abandoned` instead of success.
3. **Rebase each scope.** A clean base runs the existing rebuild, skipping
   abandoned frames, with projection, revision and outbox changes in that
   transaction. A tainted base installs the durable rebootstrap guard; the
   scope imports a clean checkpoint through the verified import. Ordinary local
   writes, including those authored on abandoned state, stay and replay.
4. **Head.** A trusted node at `(c, h_c)`, or reaching it through the endorsed
   chain, records `(a+1, running)` and may rejoin. Otherwise it stays `Behind`.
5. **Finish.** The last step deletes the work row.

Steps are range updates or deterministic rebuilds, so a crash resumes at the
cursor and re-admitting a held digest changes nothing. A returning trusted node
with an undecided entry at `a+1` resolves it as aborted by hash and voids it.

### Flow

1. A trusted node builds a `CutProposal`: the head, `(k, r_k)`, `(c, h_c)` from
   collection, and a taint report of reachable bases per scope.
2. The application shows it; the owner signs it outside any transaction.
3. The submitter flushes the record before `cut`, retries it identically, and
   publishes it only after the head names it. Rejected records are discarded.

### Failure cases

| Case | Outcome |
| --- | --- |
| Lost `cut` reply | Same record retried; same result |
| Head moved before admission | `StaleHead`; collect and sign again |
| Crash during rollback | Resume at the durable cursor |
| Abandoned frame before or after the cut | Rolled back, or consumed on arrival |
| Tainted base, clean peer base | Guarded until it imports a clean base |
| No reachable clean base | Guarded with `cut_base_missing` |
| Two records for one sequence | Coordinator admits one; a leaked other is `cut_conflict` |

## Consequences

Format identities change; none has a reader for its predecessor:

| Format | Change |
| --- | --- |
| Coordinator store/snapshot | 4 → 5: `Action::Cut`, the retained record and its accounting |
| Checkpoint statement | 9 → 10: the coordinated binding |
| Encrypted replica | One above the identity current at landing: v20 after phase 7's v19 |
| Live link | `rocky.link.v3` → `v4`: cut records and held counts |
| Row frame, bundle membership, receipt evidence | Unchanged |

Devices keep every cut record of a log, a few hundred bytes each, to fence late
frames and verify bindings. An owner who leaks two records for one sequence
breaks member convergence; Rocky detects it but cannot repair it.

### Tests

| Tier | Test |
| --- | --- |
| Unit | Coordinator cut admission and retries; record verification; range fencing; binding classification |
| Integration | Gate 2: no copy refuses appends until a cut; then a fresh node appends at `a+2` |
| Integration | Gate 3: a member returning with an applied abandoned entry reverts it; every device has the same rows |
| Integration | An abandoned effect in a pruned member checkpoint: guarded, imports a clean base, converges |
| Integration | A fault hook after each rollback step, then reopen: same rows, repeat adoption is a no-op |
| Integration | Endorsed gap beyond the window; a tampered endorsed entry is refused |
| Integration | Raft store with a cut across restart and snapshot; format 4 refused |
| e2e | Process-kill sweep over every rollback transaction |

Faults are injected at step boundaries; deadlines use the paused clock.

### Ordering

1. This record.
2. Coordinator cut, independent of devices.
3. Checkpoint binding, device cuts, fencing and rollback in one change, after
   phase 7, so replica and link identities change once.
4. Proposal, SDK native operation, Kotlin, guides and evidence.

Phase 11 does not depend on this.

### Open decisions

- **Owner key on devices.** Members do not hold the certified mesh genesis.
  Recommended: one key that is the domain's control authority and holds an issuer
  chain over every domain scope; the alternative pins genesis authorities on devices.
- **No clean base.** Recommended: fail closed and leave domain replacement to the
  application, not owner endorsement of a tainted base, which would accept
  partially applied abandoned entries.
- This refines the brief: the head moves to `a+1`, not back to `c`, and abandoned
  frames are consumed rather than refused, so author logs continue.
