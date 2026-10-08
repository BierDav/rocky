# 9. End-to-end P2P and public-coordinator qualification

Status: qualified on macOS arm64 loopback with recorded limits; real-network and Linux runs are outstanding. Owner: integration examples, runtime/coordinator evidence and
release support matrix. Depends on phases 1–8. Follow the
[execution contract](README.md#agent-execution-contract).

## Deliverable

Build a reproducible small app with two persistent device processes and a
provider-style shared coordinator. Use typed mergeable data and one explicit
strong reservation/ownership transition. One device is an eligible executor;
another may be a client/replica. An optional authorized custodian demonstrates
payload recovery separately from coordinator confirmation.

Use a packaged Kotlin application with Kotlin-owned schemas, command definitions
and deterministic handlers hosted through the supported runtime interface. The
application includes no Rust source/project or custom native compilation. Provider
infrastructure uses packaged binaries/configuration. A Kotlin RPC interface is
not automatic remote handler registration. No Room requirement.
Use local multi-process infrastructure for deterministic automation, then record
separate real-network evidence before claiming public Internet readiness.

## Acceptance scenarios

1. Register two unrelated application owners on one service. Device clients carry
   app schemas/bindings and executors carry handlers; coordinator state and traffic
   contain only bounded metadata. No per-app deployed backend is needed.
2. Offline edits to mergeable data survive restart and converge through authorized
   direct peers. The creator is not a required sync hub. Subscriptions/private
   companions retain availability semantics and no unauthorized plaintext leaks.
3. Competing commands cannot both consume one remaining item. Confirmed state is
   distinct from offline overlays; business rejection is typed and retained.
   Invalid executor, credentials, method, input type and generations fail.
4. Lose replies, cancel waits, kill/reopen executor/client and take over execution.
   Recover original tickets during the window. After retirement, report honest
   historical unavailability and reject stale identities; never retry with a new ID.
5. Attach Rocky plus a small app-owned protocol to one iroh endpoint. Close/reopen
   Rocky while the app protocol remains usable. Exercise the owned starter path
   separately. A transport-free local database still works without a coordinator.
6. Kill coordinator leader/voters, lose quorum and restore stores safely. Strong
   progress pauses; mergeable local work continues. Do not reset authority or
   infer lost payloads from commitments. Demonstrate custody-based recovery and
   explicit unavailable/loss handling when the only payload copy is removed.
7. Migrate local/replicated definitions with delayed old peers and interrupted
   backfills; key rotation/logout and checkpoint compaction retain protections.
8. Run fixed-topology traffic through many retirement cycles and session/domain
   churn. Verify bounded live and physical disk, memory and snapshot/backup peaks.
   A noisy tenant remains inside quotas without starving another.

## Fault, scale and support evidence

Keep faults deterministic: capture/signing/outbox/commit/flush failures, missing
bundle partitions, duplicate/reordered delivery, stale authority/cursors, receipt
loss, clock jumps and snapshot interruptions. Validate externally visible outcomes
and persisted reopen state, not only successful return values.

Preserve [existing scale budgets](../old/features_02/README.md#target-budgets) and
[known misses](../runtime/scale-qualification.md). Measure applicable secure and
trusted query/capture/observer/open/catch-up workloads without scanning full state
or every observer per write. Publish coordinator budgets from phases 6–7 with
tenant/domain/device counts, command rate, latency, window length, bytes and peak
compaction cost. A finite successful soak is evidence, not a proof of indefinite
boundedness; accompany it with the retention invariants and storage accounting.

Use `benchmarks/runtime-scaling` for workspace probes. Follow benchmark-specific
instructions if modifying independent engine benchmarks. Do not raise quotas,
relax targets or call unresolved platform/Internet/power-loss gates passed.

## Completion gate

- Reproduction scripts, configurations and synthetic keys are clearly fixtures;
  no provider secrets enter the repository. Tests use actual files/processes.
- Every acceptance scenario has results, failures/limits and pointers to the
  exact implementation/decision. No empty stubs, hidden background backend or
  forced application RPC/ORM is required.
- Coordinator state plateau and churn limits pass with crash/failover coverage;
  historical evidence and retirement outcome semantics remain verifiable.
- Rust `./scripts/check`, relevant native/Kotlin checks and packaged-consumer
  launch pass. Platform, Internet and physical power-loss evidence are separate.
- Current README, module map, guides and phase statuses agree on what ships,
  what remains deferred and which guarantees hold. Document public retention,
  trusted execution, outage behavior and optional custody in the starter guide.

This gate establishes a reviewable release/service candidate. Actual public
deployment needs a separate request and tested provider configuration. Room,
general RPC, alternate engines and mobile/web parity are not implied by this pass.

## Delivered behavior and evidence

The since-removed Kotlin-only market application and its historical gate ran the
eight scenarios against three provisioned
`rocky-voter` processes and persistent device processes. Owners register and control
a service domain from any language through the SDK's `command_service_*` calls.
[Decision 0029](../decisions/0029-portable-result-evidence-and-historical-results.md)
records the result-transfer corrections the run required, and the
[starter guide](../runtime/getting-started.md) documents retention, trusted execution,
outages and custody.

[End-to-end evidence](../runtime/end-to-end-evidence.md) holds the results. All 41
acceptance checks and the paced retention soak pass. Not passed or not run:

- An executor's retained-command budget is never pruned and filled after 938
  confirmed commands. Strong commands on one database are finite until that changes.
- Confirmation latency still rises with a device's retained receipts and history,
  mostly on reopen. The per-command full-history scans are removed.
- Mergeable frames from an earlier policy epoch are refused after a revocation.
- No relay, NAT or Internet run, and no Linux re-run, was made for this phase.
