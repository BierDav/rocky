# Todo MVP implementation plan

> Historical foundation plan/evidence. Implemented contracts and recorded
> limits remain valid; future sequence and ownership follow
> [architecture V4](../../proposals/architecture-proposal-v4.md) and
> [features_04](../../features_04/README.md). Old future-tense instructions do not
> override that roadmap or require optional Room work before generic services.

Status: phases 1–4 are implemented under the trusted development model.
See [strong publication](../../runtime/strong-publication.md) for coordination and recovery limits.
See [operation semantics](../../applications/todo/operation-semantics.md) for pure APIs and the
[local replica guide](../../runtime/local-replica.md) for persistence, UI and qualification. See [two-replica sync](../../runtime/two-replica-sync.md)
for delivery and recovery. The phase briefs retain their acceptance gates; this plan
does not ask an implementer to build the entire architecture proposal.
The phases were built on Turso 0.8.1; the runtime has since moved to SQLite
([decision 0007](../../decisions/0007-sqlite-engine.md)).

## Start an implementation task

1. Read the root [AGENTS.md](../../../AGENTS.md), including Jujutsu-only version control.
2. Read [development](../../development.md) and [module boundaries](../../modules.md).
3. Read the selected phase and its required contract/reference sections.
4. Inspect the owning crates with scoped `rg` searches and Cargo metadata.
5. Verify prerequisites against code and tests; a phase document is not evidence
   that an earlier phase has been implemented. Preserve unrelated working changes.

The contracts specify behavior; illustrative API names do not prescribe file
names or require a generic framework. Resolve routine implementation details
locally. Record substantial deviations and update affected briefs/contracts.

## Phases and completion gates

| Phase | Implementation brief | Completion gate |
| --- | --- | --- |
| 1 | [Operation semantics](01-operation-semantics.md) | Deterministic reducer and encoding tests pass |
| 2 | [Persistent local replica and UI](02-local-replica-and-ui.md) | Committed edits survive reopen and update Dioxus |
| 3 | [Two-replica synchronization](03-two-replica-sync.md) | Offline edits converge after reconnect and restart |
| 4 | [Strong snapshot publication](04-strong-snapshot-publication.md) | Confirmation, fencing and interrupted-request recovery work |

Phase 3 is the first usable MVP. Phase 4 extends it to exercise the second mutation
path. Both are smaller than the architecture proposal's security/coordination PoC.

Shared specifications: [operation contract](../../protocol/operation-contract.md) and
[strong-command contract](../../protocol/strong-command-contract.md). Read the strong contract
only for phase 4; earlier phases do not depend on a coordinator.

## Product and development scope

- Target macOS desktop with Dioxus, using native Rust and local Turso storage.
- One compiled todo schema, one list/domain and two fixed development node IDs.
- One window eventually shows Laptop and Phone, each backed by a separate DB.
  Phone is a simulated replica, not an iOS application or emulator.
- CRUD operations are local-first. Phase 4 publishes an immutable list snapshot;
  it does not lock the editable list or invalidate later offline mutations.
- Use `cargo run -p todo-desktop --locked -- --data-dir local/todo-demo` once
  phase 2 supplies the executable. The same command remains valid in later phases.
- Store data under the selected directory. Do not erase existing data on startup.

## Deliberate simplifications

Use fixed fixture identities and static admission rules, unsigned plaintext
operations, in-process transport and an in-memory coordinator. These are trusted
development fixtures, not authenticated capabilities or confidentiality.

Do not implement directory registration, signature/encryption integration,
p2panda networking, Raft, cloud services, mobile/WASM targets, external effects,
schema upgrades, compaction or cross-app transactions in these phases.
Keep explicit integration boundaries so later work can add them.

Retain canonical history and tombstones for this bounded demo. Do not claim a
production retention design, MVCC throughput improvement or automatic failover.
Version selection must work with the pinned Rust toolchain and be recorded in
manifests and the lockfile; do not silently switch storage engines.

## Verification and handoff

Use focused package tests during implementation and `./scripts/check` before
completion. Add tests for the phase's meaningful invariants and failures. For UI
phases, also launch the app and exercise the documented manual scenario.

Report changed contracts, check results, the run command, known simplifications
and the next phase's prerequisites. Update project/module status when code starts
implementing responsibilities currently described as empty boundaries.

## Architecture reference

The [v3 proposal](../../proposals/architecture-proposal-v3.md) was the architecture
reference. These briefs choose a reduced development path; they do not replace
its required security, durability, recovery or scaling qualification.
