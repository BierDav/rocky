# Phase 20 verification history

These are the preserved diagnostics and results before the final verification:
the SDK projection and checkpoint custody repairs, then the node-reference repair
([decision 0062](../decisions/0062-node-references-and-credential-aliases.md)).
See [current evidence](20-shopping-without-a-roster-evidence.md) for the final results.

## Historical blocker: SQL table absence (repaired)

The strict Kotlin regression initially receives code 23, `SQL session:
statement is not authorized`, for `SELECT * FROM devices`, instead of
`no such table`. The SDK installs every table in the signed logical schema
(`EncryptedReplica::install_tables`), then restricts each session's nameable
tables through its capture plan. Dropping the member's read grant protects
rows but does not remove the physical empty table. The current schema does
remove keys and identities completely.

The multi-process audit now records both native SQL errors and an independent
read-only `sqlite3` catalog inspection in `member-tables.json`. The strict
missing-table regression remains required; it is not changed to accept an
authorization refusal. Satisfying physical absence needs a generic SDK
projection contract beyond the application's schema/grant edits.

## Earlier takeover verification (2026-10-11)

The existing change `ltxyoorl` was preserved. Initial free disk space was
105 GiB. Fresh qualification evidence is under `local/p20-takeover`;
server-side diagnostic evidence is under `local/p20-diagnostic`.

All three non-gate scenarios reached their functional assertions again and
failed the strict member table-absence audit. `gate` reached merge, move and
cascade assertions, then failed horizon relay. Its member directory contained
five foreign node-key matches, in `identity.json`, `rocky.sqlite` and its WAL.
Both table and directory audits now run even after a functional failure;
secondary failures are preserved as suppressed exceptions.

The horizon's EOF was custody rejecting a checkpoint signed by read-only
member B: "checkpoint signer lacks whole-scope read and write authority".
Automatic SDK sync had finalized every readable scope, although members hold
only read on `lists`. Sync now checks checkpoint publication authority before
local finalization. Readers still acknowledge the cut, but now require a writer's
finalized checkpoint rather than signing an unauthorized one. This changes no format, session message or grant rule.
The native shopping release regression now gives B the application's read-only
list grant and write grants on categories/items, instead of a blanket writer
profile. It remains in its existing e2e tier.

The first shopping application check passed 26 JVM tests and failed two:
strict table absence and a 60-second automatic decision propagation timeout.
Both approval tests passed on an isolated rerun with the unchanged timeout.
The hand/Room required-schema preflight also passed.

## Historical blocker: read-only horizon through custody (repaired)

With the publication-authority check, B no longer emits the invalid list
checkpoint. The native regression then exposed the missing finalized-base
import: B had matching rows but no finalized checkpoint. Read-only holders now
enter the existing rebootstrap guard on receipt of finalization evidence.

The resulting diagnostic is "the peer offers no checkpoint to restore its
retained base". Custody relays the signed finalization statement and acknowledgments,
but `EncryptedReplica::checkpoint_offer` expressly returns no offer for a
keyless holder. It retains ciphertext operations, not the finalized checkpoint
pages a read-only participant needs to import. A custody-only member therefore
cannot complete this gate under the current prerequisite implementation.

The native e2e regression remains strict and fails at this boundary. No grant
was expanded, protocol or persisted format changed, test tier moved, or budget
exception added. Temporary server diagnostics were removed. The original
working copy was preserved; no push or main movement was performed.

## Earlier checks (before SDK repairs)

`./scripts/lint` exited 0. `./scripts/check` exited 0 on the final Rust tree:
146 unit tests in 104.5 ms; 839 integration tests in 138.83 s, within the
300-second suite budget. No confirmed budget miss and no new exception.
`scripts/commitlint --jj` also exited 0.

The clean serialized JVM rerun is `local/p20-verified/evidence.json`:
`gate` 36.107 s, `exclusion-revocation` 64.243 s, `relink` 59.206 s and
`creator-loss` 6.561 s. All failed the strict gates described above. The
three non-gate scenarios reached their functional assertions. The full gate
records table-absence and directory failures alongside the horizon failure.
Concurrent-build class-loading errors from an earlier rerun are excluded
from this final evidence.

Android's first responsive run enrolled both API-35 arm64 emulators and
then failed list creation with "secret store: invalid secret name". The
Keystore provider limited names to 128 characters, while the SDK's current
group epoch roles include two 64-character hashes plus the installation
prefix. It now accepts the key-provider contract's 255-byte ASCII names,
with a short independent temporary filename. Sealed bytes and AAD are
unchanged. The current native runtime was republished and the APK rebuilt.

The final two-emulator procedure passed in 17.115 s on `rocky-p20-a`
(`emulator-5558`) and `rocky-p20-b` (`emulator-5560`), Android 15 / API 35,
arm64. It enrolled trusted A and member B, closed the host issuer, delivered
both lists through custody, preserved the offline tick across the move,
merged duplicate Bread, shared from B with scanned contact A, and revoked B
through custody. Zero write retries. Evidence:
`local/p20-android-verified/phones.json`. This procedure does not exercise
horizons or the strict table/directory audit; those remain blocked above.

Desktop compilation (`./kotlin build -m desktopApp`) also exited 0.

## Historical projection and custody run (2026-10-11)

The existing working copy was preserved. `jj st` and `jj diff --stat` were
read first in the phase workspace. No main movement, push or Git CLI operation
was performed. Disk space was checked before large builds. Completed incremental
build caches were removed when space ran low; no source or evidence was removed.
One earlier cache cleanup raced with compilation and its failed native test build
was rerun; it is not qualifying evidence.

The SDK installs physical tables from read grants/selectors, keeping the complete
signed descriptor for validation. A member has no `devices`, `keys` or
`identities` table, including on reopen. Constraint indexes and generation
shadows follow this selection. New readable schema tables are installed by the
atomic projection switch. That run used identity/replica/session versions 14/13/13;
old versions are refused. See [decision 0061](../decisions/0061-readable-projections-and-checkpoint-custody.md).

Keyless custody retains finalized signed checkpoints and sealed row pages.
Uploads and custody-initiated pulls persist their cursor atomically with each
page; incomplete archives produce no offer. Referenced existence bases and
separate credential chains travel before the sealed rows. Read-only participants
import the writer's finalized cut and custody accepts their rejoined receipt.

This earlier evidence is under `local/p20-sdk-repair`. Its full multi-process gate
reached merge/move/cascade assertions, finalized all nine group-partition horizons
on A and B through custody alone, and rebootstrapped long-offline C. Physical
`devices`, `keys` and `identities` absence passed on all three members. The gate
then failed the unchanged directory audit: nine matches across `identity.json`,
`rocky.sqlite` and its WAL, representing three foreign node keys.

| Qualification scenario | Result | Wall time |
| --- | --- | --- |
| `gate` | fail: foreign node keys only | 63.923 s |
| `exclusion-revocation` | pass, including table absence | 5.949 s |
| `relink` | pass, including table absence | 5.309 s |
| `creator-loss` | pass, including table absence | 6.372 s |
| Android two-emulator `phones` | pass | 18.605 s |

The emulator procedure used `rocky-p20-a` (`emulator-5558`) and `rocky-p20-b`
(`emulator-5560`), API 35 / arm64, with rebuilt native runtime and APK. Trusted A
and member B enrolled, the host issuer closed, custody delivered both lists,
merge/move retained B's offline tick, B shared by scanning A's contact, and A
revoked B through custody. Zero write retries. Both native runtime ABIs were
republished locally and the final APK rebuilt after the contact cleanup repair.
The Android runtime build and APK build exited 0. Evidence:
`android-complete/phones.json`. Both agent-device claims were released.
This procedure does not exercise the horizon or directory audits.

The native shopping release target passed all three tests in 13.379 s under the
e2e profile (`shopping-e2e-complete.log`); its custody restart test also runs in
integration. The new driver table-absence/reopen and secure read-only checkpoint
regressions passed, including in the full tier check. The final multi-process
run (`qualification-complete/evidence.json`) exits 1 solely for directory privacy.

One JVM rerun exposed a revocation restart failure: `Settings::rotate` removed a
contact's route but left its metadata. The cleanup now removes both; a Rust
regression saves and reloads the revoked issuer. Initial workspace runs exposed
projection-generation and current-version test regressions, since fixed. Failed
and contended diagnostic runs are not substituted for passing checks. No
exception was added.

## Checks before the node-reference repair

`./scripts/lint` exited 0. The final `./scripts/check` exited 0
(`check-final.log`): Clippy passed; 146 unit tests passed in 121.8 ms and
844 integration tests passed in 161.19 s, within their 10 s / 300 s suite budgets.
It includes the SDK without default features. Builds used
`CARGO_PROFILE_DEV_DEBUG=0`, `CARGO_PROFILE_TEST_DEBUG=0` and bounded build jobs
to reduce disk use, retaining unoptimized code and debug assertions.
The physical table regression's independent run passed in 530 ms; the secure
checkpoint regression passed in 547 ms; the contact revocation/reopen regression
was confirmed alone at 91.7 ms. All retain the normal integration filter.

The final shopping check exited 0 (`shopping-check-final-verified.log`): Room/hand
required-schema preflight, all 28 JVM tests in 28.015 s, and desktop compilation.
The largest concurrent JVM case was issuer recovery at 20.523 s. Kotlin tests
are not enforced by `scripts/test-tiers`; this passing suite does not establish
that every Kotlin integration case meets the two-second per-test budget.
The approval fixture now explicitly synchronizes to its committed decision under
a timeout instead of waiting indefinitely after one manual pass.

No budget exception was added. Earlier contended Rust attempts confirmed
`strong_commands_and_incoming_replication_share_the_file_writer` at 8.31 s and
`a_rowless_checkpoint_imports_into_one_domain_of_a_shared_file` at 22.82 s;
the final full check confirms the Rust integration budgets without new exceptions.
A later check passed all tests but exited 1 for the existing relay case at
2.03 s alone. Starting its two independent endpoints concurrently reduced the
final confirmed time to 701.4 ms, retaining relay-only and ownership assertions.
The final isolated Kotlin roster cases passed: missing tables 589 ms, contact
handover 1.036 s and scanned sharing 2.771 s. Sharing still misses the two-second
integration budget. Its fixture opens three Room-backed installations and drives
key delivery plus several sync passes, including with the issuer offline. An earlier fixture measured 3.112 s alone. Using an in-memory secret provider
retains all sharing and offline assertions, but this miss remains. No assertion,
tier or budget was relaxed. The full
concurrent JVM suite's larger measurements are reported above; the other Kotlin
cases have not all been confirmed alone. The isolated approval cases also
passed: registry-backed names/decision delivery in 1.126 s and the existing
member/first-decision case in 1.149 s (`approvals-final-alone.log`).

## Historical blocker: literal directory privacy (repaired)

The completion gate requires a byte search of a member's entire database and
installation directory to find no other device's node key. Phase 14's current
SDK persists local contacts by node key in `identity.json` (`Settings.peers`,
`contact_meta`, `issuers` and enrollment routes). `Settings::save` serializes
these fields directly as JSON. Credential chains and durable session evidence
can also name another node. A device keeping even the one custody/issuer
contact its accepted flow requires therefore cannot satisfy that literal gate.

The current task explicitly authorizes SDK repairs, including current format
changes. Removing the durable foreign keys remains unimplemented across local
contacts/issuer routes and credential/horizon evidence. This implementation
keeps the literal audit in the full harness gate, records its
matches in `member-privacy.json`, and reports failure instead of silently
excluding SDK files or weakening the gate to application tables. The audit
checks foreign node keys as hex and raw bytes; a node-key match already fails
acceptance, before name/kind/envelope privacy could establish a pass.

The fresh audit's exact JSON paths are `Settings.peers`, `contact_meta` and
`issuers`. Independent row inspection also finds those keys in
`secure_strong_base`, `secure_checkpoints`, `secure_checkpoint_strong`,
`secure_peer_frontiers`, `secure_horizon_credentials` and `secure_chains`. The locations are saved in
`qualification-complete/foreign-key-locations.json`. Removing contact settings
alone would leave these matches and remove the routes needed after restart.
There is no encoding, encryption or audit-exclusion workaround in this change.

The continuation traced the coupling to the authentication contracts:
`NativeSession::authenticate` pins the contact's public node key;
`CredentialChain::links` verifies each issuer key and signed capability;
`HorizonStatement.peers` and `HorizonAck.node` name public node keys under
signatures. `record_horizon_chain` and `secure_peer_frontiers` retain those
identities for horizon and custody safety after restart. Deleting the fields
would lose authenticated routing or invalidate signed evidence. A complete
repair needs durable contact addressing and verifiable participant evidence
without foreign node keys across all of these boundaries. That repair remains
unimplemented. The source trace is `local/p20-sdk-repair/privacy-contract-trace.md`.

## Continuation before the node-reference repair (2026-10-11)

The fresh `qualification-continuation/evidence.json` confirms the functional
gate through all nine custody-only horizons, long-offline rebootstrap and
physical table absence. It then fails the unchanged full-directory audit with
nine matches for three foreign node keys. Gate wall time was 205.011 s;
exclusion/revocation, relink and creator-loss passed in 13.065 / 16.662 / 16.605 s.
No SDK or application source changed in this continuation. The preserved
two-emulator qualification therefore still covers the same Android sources;
Phase 21's emulators were left alone.

`lint-continuation.log` records a passing lint. The final shared check
(`check-continuation-final.log`) passed Clippy, 146 unit tests in 116.9 ms and
844 integration tests in 186.68 s, with unchanged budgets and filters. The
first run failed with a 30.824 s strong-cut timeout and a 24.78 s retention
budget miss while disk space fell below 1 GiB. Only completed incremental
caches older than 30 minutes in this workspace were cleared, restoring 15 GiB.
The two cases passed alone in 492 / 369 ms, then in the full check. The prior
Kotlin scanned-sharing budget miss above remains; it was not reclassified.
