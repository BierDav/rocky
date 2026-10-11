# Phase 10 qualification

Implementation date: 2026-10-10. Assigned workspace starts at main `375958db`.
[Decision 0052](../decisions/0052-holding-proofs.md) records the full message audit,
derivation and native session version 10. `identity.json`, group envelopes,
epoch identities and stored replica formats are unchanged.

## Acceptance coverage

`rocky-secure` `network::holdings::tests` runs over authenticated in-memory
streams and real SQLite replicas. The tier checker places it in integration:

- A profile reader without a key probes an existing partition and a random
  scope against a reader and a keyless custodian. Pull (including its pruned
  branch), Range, Held, Push, FrontierQuery, checkpoint offer/pull, Read,
  every horizon request, Execute (including a partition invocation with an
  unpartitioned result subscription) and result pull/push produce identical server
  errors, no response messages and EOF. Existence requests also meet this gate.
  Frontier batches return only the request's `Refused(scope)` and continue.
- Scope enumeration omits an unproved partition that is demonstrably present
  in the old grant-only local listing. The continuation cursor is empty.
- A captured proof succeeds in its session, a subsequent scoped request needs
  no second proof, and replay in a fresh session is refused. Five proofs fail
  before serving or registration.
- A holder of only the previous generation, then only the losing sibling,
  is served by a peer retaining it and refused by a freshly bootstrapped peer.
  The client emits only four newest proofs from a longer retained history.
- A custody-only peer receives a new tuple offer and stores a signed frame
  without a key; it then proves to a reader with its tuple and grant. A peer
  granted another list gets neither disclosure nor registration, even with
  a valid key proof and the offered tuple.
- A profile reader cannot probe a retained frame ID with an unserved ACK;
  retained and unknown IDs produce equal denials. A proved reader can pull
  and acknowledge normally, including frames served by Range.

The pure `network::holdings::primitives::proof_binds_key_scope_epoch_and_session`
test stays in unit. The full-size
`tests::group_sync_measurement::holdings_idle_200_partition_four_table_fixture`
is an ignored e2e gate explicitly selected by `.config/test-tiers.toml`, so
`scripts/test-tiers e2e` and CI's e2e job run it. It uses the existing four-table,
200-partition fixture, verifies rows/references, reopen and checkpoints, and
asserts an authenticated idle round with proofs below 10 seconds.

## Measurements and checks

Host: Mac17,8 / Apple M5 Pro, 18 cores, macOS arm64; Rust 1.99.0
(`b940084d7`). The repository's test profile includes its existing secure-package
optimization. The host was shared with other builds.

The targeted in-process test binary ran each test alone with `--test-threads=1`
and libtest's `--report-time`. All seven passed; the pure proof-binding test took
about 2 ms, and integration scenarios ranged from 71 to 208 ms. Later additions
exercise both command dispatchers, the sealed invocation scope, and proved versus
unproved scope enumeration; the final tier checker covers these additions.

The final full-size fixture passed under nextest's e2e profile in **32.999 s**. It
registered/authored 801 scopes/frames and caught up in one session. The idle round
also used one fresh authenticated session, carrying proofs for 800 table
partitions; it took **1.001 s**, below the **10 s** gate. Other fixture timings:
registration 1.631 s, authoring 2.825 s, catch-up 18.842 s, 801 checkpoints 1.559 s,
reopen 0.957 s. These measurements include real SQLite and loopback TCP.
An earlier run under contention also passed, with a 1.884 s idle round.

Final `./scripts/lint` passed (exit 0). Final
`NEXTEST_TEST_THREADS=4 ./scripts/check` passed (exit 0), including formatting,
workspace clippy with warnings denied and both test builds. Unit: **135 passed**,
suite **70.5 ms** against 10 s. Integration: **750 passed** (593 workspace plus
157 SDK without default features), suite **118.41 s** against 300 s. All
per-test budget checks passed. The six holdings integration scenarios took
**63–248 ms** in this run. Coordinator fairness took **4.71 s** alone within
its existing 5 s allowance. Earlier shared-host runs failed or were stopped;
they are not passing evidence. No new exception or larger limit was added.

The targeted e2e run passed all **three** selected tests (exit 0) in **38.365 s**:
the holdings fixture, owned-iroh convergence (**2.689 s**) and executing-device
service registration (**2.675 s**). It did not run the full e2e suite.

The full check found existing tests beyond the integration budget. The
revocation recovery sweep now separates secret, table and commit boundaries;
the same twenty stops and two real reopens per stop remain, using the same
prepared-database helper. Transport fixtures retain in-memory keys across
reopen, close independent peers concurrently, and verify convergence after
one sync round instead of repeating it. The executing-device service scenario's
duplicate signing-clock wait is covered by its existing service e2e test.

Two existing owned-QUIC lifecycle scenarios are classified as e2e, rather than
given exceptions: `devices_converge_over_iroh` measured **2.705 s** and
`a_device_granted_execute_registers_and_executes_without_being_an_issuer`
**2.639 s** alone after optimization. Stage traces measured about **1 s** for
each owned endpoint shutdown cycle; the restart scenario needs two cycles,
and the service scenario additionally drains its voter runtime. These real
transport shutdown waits follow the existing e2e tier's production-deadline
rule. The tests still exercise real loopback iroh, SQLite and the service.
The borrowed-endpoint integration fixture overlaps independent final shutdowns
while retaining its ownership/reopen assertions. The coordinator fairness
fixture registers independent tenants and starts independent flood clients
concurrently; its existing 5 s exception is unchanged. The pseudonym fault
fixture closes its automatic sync task before injecting an identity-write
failure and reattaches afterward, preventing a race in fault setup. No new
exception or larger limit is introduced.

## Boundaries

The fixture runs on loopback TCP; the adversarial transcripts use in-memory
streams. No WAN, relay, physical phone, packaged Kotlin application or hosted
CI run is claimed. The full e2e suite and opt-in doc tests were not run.
The baseline checks also do not establish those profiles.
No cryptographic audit is claimed. Tuple guessing and retained tuples of former
readers remain the deliberately weaker custody boundary; contacts, key delivery
and horizon relay belong to later phases.
