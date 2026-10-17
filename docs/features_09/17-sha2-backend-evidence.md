# Round48 SHA-2 backend experiment

Status: fresh HIGH49 accepts the actual backend code, and current-source
baseline and package checks pass. The one 30,000-row comparison fails the
continuation rule (2026-10-21). Performance work stops; qualification remains open.
The accepted round47 author-payload refactor is retained.

The owner's subsequent scope decision permits phase20 to proceed: the exact
100,000-row migration test is exempt from the combined suite clock, and Linux
ARM fallback execution is not a release blocker. Both remain accurately
reported below; this does not turn the old timing run into a pass or claim
fallback execution. No further backend performance experiment is required.

## Production boundary

`rocky-core` keeps ordinary SHA2 0.10 defaults. A target dependency overlay
enables its upstream `asm` feature only for little-endian AArch64 Apple, Linux
and Android targets. SHA2 stays at 0.10.9; the unavoidable new sha2-asm package
is 0.6.4, with checksum
`b845214d6175804686b2bd482bcffe96651bb2d1200742b712003504a2dac1ab`.
The root, native, discovery and chaos locks add only this package and its SHA2
dependency edge. The external measurement lock has the same mechanical change.
All existing package versions, including cc 1.6.0, remain fixed.

Cargo unifies the feature across the active target graph, including credentials
and Ed25519's SHA512. It does not apply only to checkpoint hashing. Single-target
native graphs show asm on the five supported ARM64 targets and no force-soft
feature. The x86_64 macOS/Linux/Android/Windows graphs retain their previous
backend. Windows ARM64, big-endian AArch64 and wasm graph inspections also show
no activation by this overlay; these inspections do not establish SDK support
for those platforms. A downstream dependency can change the feature union.

No hash algorithm, labels, lane count, block indices, canonical input, public
API, format, verification, profile or compiler tuning flag changes. Formats
remain file32/link14/checkpoint16/row11/backup2.

## Independent bytes and reached mechanism

Before editing the backend, Python hashlib supplied fixed core commitments over
explicit u32 little-endian UTF-8 prefix lengths, raw SHA256 and raw SHA512 answers,
including padding and multi-block boundaries. The fixed genesis answer remains.
Actual software production encoders/signing captured deterministic Materialized
and checkpoint bytes. Python independently expanded all 1,024 lanes and checked
Sets states, checkpoint roots, signing envelope and checkpoint ID.

The three secure tests check every lane byte, add/remove/order/multiplicity with
wrapping, disjoint merge, actual Sets replacement/removal/merge and both summaries.
They reproduce the complete signed statement, signing bytes, signature and ID,
verify the old fixture and reject root/signature tampering at the real gate.
See the [fixture provenance](../../crates/rocky-secure/src/replica/checkpoint/backend_vectors/README.md)
and its standard-library-only verifier. The software and normal candidate runs
both pass the same frozen assertions. Expected answers are not manufactured by
the production helper during tests.

The normal candidate core KAT executable calls SHA2's compression functions;
disassembly of those functions contains SHA256H/H2/SU0/SU1 and SHA512H/H2/SU0/SU1.
The M5 Pro reports SHA256, SHA3 and SHA512 capabilities. Pinned upstream source
selects the AArch64 module only with asm; cpufeatures treats Apple ARM64 SHA2 as
present, and this unmodified Rust target already advertises sha2/sha3. No added
target-feature flag or forced software feature is involved. SHA512's separate
SHA3 decision is covered by raw vectors and the deterministic Ed25519 fixture.
This proves reached mechanism, separately from elapsed performance.

No qualifying nonaccelerated Linux AArch64 runtime was established. The
normal runtime must report HWCAP_SHA2 absent and both HWCAP_SHA3 and
HWCAP_SHA512 absent: pinned cpufeatures tests the latter two as an OR.
Both fallback executions remain unexecuted. Cross-linking does not close
that gap. Apple fallback is not claimed tested.

The one bounded infrastructure attempt provisioned isolated QEMU 11.1.2 and
inspected actual CPU properties and pinned source. The checked models expose
SHA2 without an independent SHA control; removing Neon also removes floating
point and does not establish a compatible normal GNU runtime. No qualifying
Linux guest or actual AT_HWCAP evidence was obtained. No fixture cross-build,
forced-soft option or compiler workaround followed. This is an unavailable
runtime prerequisite, not proof that every emulator route is impossible.
Commands, bottle/source hashes and terminal evidence are in
`/tmp/rocky-phase17-linux-arm-fallback-infrastructure-49.md`.

## Focused checks and native coverage

The core vector/genesis tests pass before and after the overlay. The candidate
passes 95 selected secure controls, including all three new byte tests, and all
72 credentials tests. The selection retains checkpoint/replay, own-zero carry,
claims/receipts/proof-first tamper, rotation, held/privacy/finality and author
retry boundaries. Each selected secure test stays below 2 s. The scoped tier
checker passes 100 unit tests within its original budgets; these counts overlap
other selections and must not be added as distinct tests.

Core/credentials/secure all-target locked Clippy with warnings denied passes.
SDK no-default two-step migration passes in 0.989 s; the default-iroh real-voter
private migration passes in its proper e2e profile in 4.555 s.

All 11 normal native builds/links pass across nine Rust targets: macOS ARM64
and x86_64; iOS device and simulator ARM64, each with and without iroh;
Android ARM64 and x86_64 in the existing release profile; Linux ARM64 and
x86_64; and best-effort Windows x86_64. The existing `build-desktop`, `check-ios`
and pinned NDK compiler/archiver configuration were used without tuning flags.
Lint and JJ commitlint pass. Cross-built libraries establish linking, separately
from execution on those targets. Native inventories and hashes are retained in
`/tmp/rocky-round48-native-artifacts.json`; final packaging/consumer execution
is still a qualification gate.
Contributor assemblers are covered by the existing native workflows;
[packaged consumers](../runtime/sdk-packaging.md) still need no Rust/C/assembler.
No remote publication or phase20 artifact/device work is part of this experiment.

## One comparison and remaining gates

The external helper, manifest, feature requests, profile, SQL, 30,000 rows,
one-sync and all original generation/data/fingerprint/accounting assertions
remain unchanged. Its existing driver has only a new output prefix and retains
the same three bounded fill captures and 180 s test-only watchdog excluding
compilation. No repeat is authorized by a flat result.

GO requires fill <=34.888397375 s, total <96.41 s and no meaningful offset in
other stages, with correctness, mechanism and available builds green. A useful
result requires fresh actual-delta review before ROOT's one canonical100k gate.

The prior 97.36 s immediate baseline has seed/additive/fill/member stages
11.984007167/7.694335500/37.677526333/38.894320083. The unchanged CODE43 baselines
are 107.07 s (12.801521750/7.883367750/36.888397375/48.359616333) and 96.41 s
(12.438401333/7.424431250/37.199207834/38.334470458). Their variation is not an
optimization result.

The one candidate run passes in **96.53 s**, with seed/additive/fill/member
**11.209071125/7.129321667/33.122666291/44.055295292 s**. ONE sync and all original
generation, exact-data, fingerprint and accounting assertions pass. The normal
peer-closed report precedes successful completion; no extra sync or retry was
inserted. Measured source is `eb46e3e33bfa014510b06d38cba315ef55759287`; subsequent
changes are documentation only. The helper/manifest are byte-identical, and the
external lock differs only by the reviewed dependency maintenance.

Fill is 3.765731084 s below the fastest named fill baseline and 4.554860042 s
below round46. But member pull is 5.720824834 s above the 96.41 s reference
(5.160975209 s above round46). Total is 0.12 s above the required reference.
**NO-GO:** fill meets its threshold, total and the no-offset condition do not.
This is no end-to-end timing repair, despite correct hardware selection.
No second 30k run, 100k run, profiling campaign or full matrix was launched.

The unchanged three captures ran at +2.058/+14.377/+26.149 s after the approximate
additive marker. Early/middle reach actual migration authoring, including
`write_materialized -> Sets::replace -> Lt::mix -> commitment`; late reaches
finalization/restoration/replay. They do not measure seconds saved or explain
the member-stage offset. No further optimization is authorized from these
inclusive samples.

Full commands/timings/exits/log paths are in `/tmp/rocky-round48-commands.jsonl`;
`/tmp/rocky-round48-comparison.json` retains all four stages, baselines, exact
accounting and unchanged-input hashes. The implementation handoff is
`/tmp/rocky-phase17-round48-implementation-handoff.md`.

Fresh HIGH49 accepts snapshot `1f55a2aa6b1a` without a P0/P1/P2 finding and
preserves performance STOP. Current-source lint, commitlint, ordinary workspace
checks, native/Kotlin checks, clean packaged-JVM consumer, local Maven and
shopping checks pass; [the qualification record](17-schema-changes-through-seals-qualification.md#round49-acceptance-and-current-source-checks)
records exact counts and timing. The bounded service subset passes its three
existing cases, including P5, without reproducing the earlier failure.

Matrix10's P5 `counts Incomplete` failure at 13.873 s remains cause unknown;
later passes are not a fix. Its actual combined 990.38 >900 failure remains.
Both canonical e2e builds, the required fallback executions and phase20 remain
open. This experiment neither accepts all features09 nor starts phase20.
