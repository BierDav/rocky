# 8. Recovery, generation upgrades and composable SDKs

Status: implemented for the bounded encrypted single-file and Kotlin/JVM desktop profiles. Owner: runtime/secure migration and recovery, coordinator
generation transitions, `rocky-sdk`, native ABI and Kotlin core. Depends on phases
3–7. Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [old migration requirements](../old/features_03/06-migrations-and-recovery.md),
[old SDK requirements](../old/features_03/08-native-sdk-and-lifecycle.md),
[recovery/effects](../runtime/recovery-and-effects.md), [SDK contract](../runtime/sdk-contract.md)
and [scaling/rebuilds](../runtime/scaling-and-rebuilds.md).
Deliver recovery and language APIs for the accepted modular profiles, without
requiring a Room driver or universal platform support.

## Ordered slices

1. Separate local projection/index migrations from replicated identity/schema,
   handler and reader-policy/key changes. Define signed compatibility transforms,
   old-peer admission horizon and pending operation/command treatment. Preserve
   original signed bytes; unsupported transforms fail before generation activation.
2. Record durable generation plans, bounded backfill progress and verified shadow
   projections. Atomically switch local visibility for one placement group;
   observers never see half a rebuild. Replicated/invariant changes require
   corresponding verified state commitments and authorized coordinator activation,
   not a bare generation-number bump. Network calls remain outside local leases.
3. Extend checkpoints/rebootstrap to typed identity, tombstones, bundles, ticket
   outcomes, retained signed receipts and phase 6 retirement/session evidence.
   Compaction cannot discard the sole recovery evidence for an admitted local
   outcome; public-window expiry instead yields explicit historical unavailability.
4. Define coherent backup and fresh-device restore with new signing identity.
   Preserve uncertain commands/effects under their original identities and never
   redispatch them as fresh work. Missing payload/key and acknowledged-loss domain
   replacement retain their existing explicit/fenced behavior.
5. Expose typed database sessions, credential providers, command tickets/results,
   network attachment, application-language handler registration and coordinator
   configuration as independent native/Kotlin APIs. Starter setup composes them.
   No raw writer/file/pointer or per-business-repository sync queue is required.
   Foreign cancellation and close preserve
   committed work and do not shut down borrowed endpoints/shared sessions.
6. Provide bounded batches/cursors/errors and handle generation ownership.
   Kotlin Flow reflects committed data/progress, including readiness/authority
   changes. Define reconciliation after an interrupted foreign acknowledgment;
   a new DAO/app invocation is not automatically the same business intent.
7. Package bindings/native resources for the qualified JVM desktop target with
   clean-consumer launch. State CPU/OS versions and migration path from the current
   SDK facade. App developers need no Rust/Python for packaged consumption, schema
   definitions or handler implementation. Document adapter contracts and generation tooling
   so another language can integrate without changing the protocol or rewriting
   business logic in Rust.

Room-specific synchronous driver scheduling, catalog emulation, invalidation
tracker and existing Room-file import remain deferred adapter work. Android,
iOS and web need separate lifecycle/key-storage/binding qualification; desktop
success cannot establish them. Cross-coordinator migration remains deferred.

## Completion gate

- A local projection migration and a declared replicated/handler upgrade retain
  keys/identities and pending work. Interrupt every plan/switch/activation boundary;
  resume the same plan or reject incompatible work without partial visibility.
- Old devices outside the support horizon rebootstrap or fail explicitly; no
  historical operation is re-signed or admitted under invented current authority.
- Fresh-node restore preserves typed queries, private scope availability, retained
  outcomes and bounded coordinator retry semantics without forking author logs.
- Native and Kotlin consumers use database/credentials/commands separately and
  together; closing, renewing, cancelling and reopening preserve ownership and
  durable identity. No ORM is required for the examples.
- A clean packaged consumer starts on each claimed target with no build tools;
  supported feature/platform and old-SDK migration matrices are published.
- Rebuild/restore/reopen never sends external effects merely by replaying data.

Run migration/checkpoint/recovery/subprocess tests, native/core Kotlin checks,
consumer packaging checks and `./scripts/check`. Update owning guides and evidence.

## Delivered behavior and evidence

Signed generation lineage, bounded verified projection plans, historical admission
horizons, original-identity pending work and authorized exact-head coordinator
activation are described in [generations](../runtime/generations.md).
[Backup/restore](../runtime/backup-and-restore.md) preserves typed state and durable
outcomes while enrolling a fresh signer. Native database/session/network handles
and Kotlin's local/openSession/credentials/commands/attach APIs share the starter's
runtime. Readiness Flow and durable-handle reconciliation cover interrupted foreign
acknowledgments. [SDK packaging](../runtime/sdk-packaging.md) provides artifact-only
consumption, the old-facade migration and another-language adapter contract.

[Qualification results and platform limits](../runtime/recovery-sdk-evidence.md)
record workspace/native/Kotlin checks and the clean macOS arm64/Linux x86_64
consumer gate. Unsupported transforms fail explicitly; reader-scope remapping,
Room driver/import, arbitrary cross-file atomicity, mobile/web and cross-coordinator
migration remain deferred. No Maven/native artifact publication or live public
service deployment is implied.
