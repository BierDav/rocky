# 6. Always-on peer

Status: slices 1–3 implemented and locally qualified at the handover boundary;
phase incomplete. Partition-grant slice 4 awaits phase 11; final integration
qualification is deferred. Owner: custody replica packaging and `rocky-sdk` sync
scheduling. Depends on nothing to start; after [phase 11](11-partitioned-scopes.md)
the custody node must also verify partition grants. Wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 9, item 2](agreed-scope.md#9-platform-and-operations) of the
scope, [replica roles](../runtime/replica-roles.md),
[iroh network](../runtime/iroh-network.md) and
[coordinator deployment](../runtime/coordinator-deployment.md) for the
packaging pattern. Inspect the SDK's 500 ms sync polling.

Two phones are rarely online together, and pairing and renewal need a
reachable issuer. Package a custody-only replica that can run unattended, and
replace polling with sync driven by local changes and connectivity.

## Ordered slices

1. Package the custody-only replica as a container image beside the
   coordinator image, with configuration for enrollment and peers and a
   runbook.
2. Change-driven sync: a local commit, an incoming frame and a connectivity
   change trigger sync; idle devices do not poll. Backoff and retry stay
   deterministic under the controllable clock.
3. Renewal and pairing through the always-on peer when the issuer phone is
   offline, within what the credentials allow today; issuer rights on that
   peer are [phase 10](10-issuer-recovery.md).
4. After phase 11: the custody node checks the signature and the grant on the
   partition scope and never needs the partition key.

## Completion gate

- Two devices that are never online together converge through the custody
  peer, which cannot read the rows.
- No periodic polling in an idle process; a change reaches a connected peer
  without waiting for a timer.
- The packaged image runs from configuration alone and its runbook covers
  enrollment, backup and replacement.
- [Replica roles](../runtime/replica-roles.md) and the deployment guide
  describe the always-on peer.

## Measured evidence

- `cargo test -p rocky-secure --locked custody`: four prerequisite custody,
  durability, pruning and eviction tests passed before editing.
- `cargo test -p rocky-credentials --locked`: all nine tests passed, including
  custody-only grants without read, write, executor or issuing rights.
- `cargo test -p rocky-sdk --locked --test credentials`: six tests passed.
- `cargo test -p rocky-sdk --locked --test custody`: two tests passed: staggered
  devices converge through keyless custody after relay restart, and TCP
  configuration enrolls, backs up and resumes while refusing reader enrollment.
- Container build initially exposed missing `libclang` for SQLite bindgen; the
  peer builder installs it. `docker build -f crates/rocky-sdk/Dockerfile -t
  rocky-peer:p06 .` passed on linux/arm64. The explicit ignored container test
  passed: configuration-only resume, public contact, SIGTERM drain and native
  reopen of the resulting custody file. No amd64 build, registry publication
  or real-network run was performed.
- Pairing and renewal require publisher signatures today. Custody grants
  cannot sign either while all issuers are offline; phase 10 owns issuer rights.
  Slice 3 now forwards to a separately authorized reachable issuer and qualifies
  the all-issuers-offline refusal; it grants no issuing rights to custody.

- Native scheduling tests passed: a day of paused idle time causes no dials;
  a local commit reaches a peer without a polling timer; committed keyless
  receipt wakes relay forwarding; connectivity wakes sync; failed work retries
  on the controllable clock and connectivity interrupts backoff. The retry
  unit test measures 1/2/4/8/16/32/60-second deadlines and no idle deadline.
- `cargo test -p rocky-sdk --locked`: all non-ignored tests passed before the
  observer wakeup refinement; targeted readiness, engine and library tests
  passed after it. `cargo test -p rocky-secure --locked`: 104 passed, five
  measurement tests ignored, including duplicate custody receipt wake checks.
- `./sdks/kotlin/check-packaged`: packaged JVM consumer passed, including native
  loading, sessions, handlers/tickets, owned/borrowed networks and recovery.
- `./scripts/check` passed for the first scheduler snapshot (previous-agent
  evidence). It was intentionally not rerun at handover; final workspace
  verification remains required.
- External coordinator head changes need an application sync/connectivity
  signal; no new coordinator watch protocol was added by this phase.

## Handover boundary (2026-10-07)

- `cargo test -p rocky-sdk --locked`: 80 passed, zero failed, three ignored
  (two measurements and the Docker test); doc tests had zero cases.
- `cargo test -p rocky-sdk --locked --test custody --test credentials --test peers
  --test generations`: 20 passed, zero failed, one Docker test ignored.
  `credentials` is the enrollment qualification suite; no separate `enrollment`
  test target exists.
- `cargo clippy -p rocky-sdk --all-targets --locked -- -D warnings` and
  `cargo fmt --all -- --check` passed.
- `./sdks/kotlin/package-sdk` passed: JVM core library and packaged consumer
  compiled, and SDK-owned native preparation completed.
  `local/packaged-sdk/run local/p06-packaged-consumer` passed local sessions,
  progress Flow, reconciliation, handlers/tickets, owned/borrowed networks,
  fresh restore and generation reopen. Java emitted a JNA native-access warning.
  This does not qualify forwarded enrollment through Kotlin or a newly built
  container. No container rebuild or run was attempted at this boundary.
- The forwarding test pairs with the creator offline and an explicitly enrolled
  issuer online, renews without changing grants, renews after custody restart,
  and observes retryable `unreachable` after the remaining issuer closes.
  Custody retains neither reader keys nor issuing authority. Format tests cover
  routed invitation/contact authority fields and version-6 to version-7 migration.
- Actual phase history includes custody packaging `tltqszpp` and scheduling
  `sqmwzwln`. Their base is roadmap change `zsoxpxrp`, not current main
  `npmturto`; integration with the phase-04 landing has not been performed.

Required result is **not met at this handover**. Next run should integrate the
preserved phase chain with current main, review and qualify the packaged
forwarding path (configuration, JVM API and supported transports), then run
`./scripts/check` at the final boundary. Preserve the no-idle-polling,
commit-before-publication and application-owned-network lifecycle contracts.
Slice 4 must wait for phase 11: validate both signature and partition-scope
grant without obtaining the partition key. Issuing-right provisioning remains
phase 10; no reachable issuer means pairing/renewal must refuse, never succeed.
No further phase-6 slice was implemented during this handover.
