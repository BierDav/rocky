# Phase 20 evidence: shopping without a roster

Workspace `p20-shopping-without-a-roster`. The Phase 20 change is rebased onto
main `8ffd2ed4` (Phase 21 iOS and the market removal), above a separate change
that drops the deleted market modules from the Kotlin build. No push, bookmark
or main movement. Evidence paths are under `local/p20-sdk-repair`.

Status: complete. Every [completion gate](20-shopping-without-a-roster.md#completion-gate)
item passed on the final tree; open observations are listed at the end.

## Current application

Room/schema version 6 contains lists, categories, items, requests and decisions.
The overlay adds trusted-only devices, invitations and coordination. No keys
or identities table is installed. Member grants name only the five Room tables.
Trusted nodes read command identities and device descriptions from the issuer
registry, with the trusted description table as a fallback. Registry labels
carry only name and kind, bounded to 256 bytes.

A member's Share list action scans a recipient contact; the trusted picker
reads the registry. Member device names are local labels and approvals show
This device or Another device. Sync displays contact source, ranking reason
and last connection state. Pairing needs one contact and authenticated handover,
without discovery or reciprocal contact entry. No application roster horizon
is declared.

## Tests and tiers

`RosterFreeTest` exercises real SQLite, Room and TCP; it belongs to Kotlin
integration alongside `HouseholdTest` and `ApprovalsTest`. The required-schema
preflight imports Room's version-6 export with the application overlay.
Shopping qualification is multi-process e2e in the existing shopping CI job.
The Android procedure now enrolls B as a member, shares from A's registry,
shares back by scanned contact, and revokes B. `scripts/test-tiers` checks
Rust only; no test-tier exception was added.

Rust unit/integration checks retain their existing filters and budgets. The
Kotlin sharing fixture uses a thread-safe in-memory secret provider to avoid
repeating file-key durability work; SQLite, Room and TCP remain real. It retains
its sharing and issuer-offline assertions. Budget misses, including Kotlin
measurements, are reported below without an exception. The iOS pair/share/revoke
steps are e2e in `qualify-ios-pair`, beside the Android procedure.

## Final verification (2026-10-11)

| Check | Result | Evidence |
| --- | --- | --- |
| `./scripts/check` | exit 0: 147 unit tests in 111.4 ms, 854 integration in 142.46 s (budget 300 s) | `privacy-check-final-22.log` |
| `./scripts/lint`, `./scripts/commitlint --jj` | exit 0 | `privacy-lint-final-22.log` |
| `./examples/shoppinglist-app/check` | exit 0: required-schema preflight, 30 JVM and 2 Android unit tests, desktop build | `privacy-shopping-check-22.log` |
| Shopping JVM suite, four consecutive runs | 30/30 each, 18.7–18.8 s | `privacy-shopping-tests-22-{1..4}.log` |
| Harness `gate`, `exclusion-revocation`, `relink`, `creator-loss` | pass: 30.079, 5.936, 5.102, 6.276 s | `privacy-qualification-final-22/` |
| Android `phones`, `emulator-5580`/`5582`, API 35 arm64 | pass in 22.065 s, zero write retries | `privacy-android-22/phones.json` |
| `./examples/shoppinglist-app/check-ios` | exit 0; iOS integration test 0.974 s | `privacy-check-ios-22.log` |
| `qualify-ios`, iPhone 18 Pro Max simulator, iOS 27 | pass in 36.98 s, eight expectations | `ios-simulator-22/simulator.json` |
| `qualify-ios-pair`, that simulator and `emulator-5580` | pass in 32.5 s | `ios-pair-22/expectations-shopping.json` |

The Android runtime, APK, iOS libraries and simulator app were rebuilt from the
final tree. `agent-device` needs `node`; this host has none, so the iOS runs used
a temporary `node` shim to Bun on `PATH`.

After the full `gate`, every file in member B's directory is searched. It holds no
node key of A, C or custody (hex or raw), neither C's name nor its full
`{"name","kind"}` registry label, and B's delivery state names no envelope
recipient. A control requires each name in the trusted device's directory; A's
`rocky.sqlite` holds it, while registry labels are sealed at rest. A bare kind
word is not searched, since the schema contains it; the label bytes cover kind.
Command identities in `requests` and `decisions` are the stated exception.

Gate items: harness scenarios and table absence (harness, `RosterFreeTest`);
member sharing by scanned contact and trusted sharing from the registry to a
device it never contacted (`phones`, `qualify-ios-pair`, `RosterFreeTest`); one
contact hands over a second device with discovery disabled (`RosterFreeTest`);
approvals decided by a trusted device that never synced a roster table
(`ApprovalsTest`); custody-only horizons on members (`gate`); two emulators
(`phones`); docs (README, [getting started](../runtime/getting-started.md),
[audit](../runtime/shopping-qualification.md#phase-20-audit)). Phase 21 had
landed, so the iOS pair/share/revoke steps are in
[shopping on devices](../runtime/shopping-devices.md#ios-procedure) and its
simulator check was rerun.

## Defects fixed in the final continuation

- **Stack overflow.** `behind_horizon_sync_restores_the_base_and_preserves_unsent_work`
  aborted on the SDK's 2 MiB database thread. Measured with lldb, the deepest
  frames were `database::dispatch` (314 KB) and `sync::reconcile` (193 KB). Both
  futures are boxed at their call sites, as saved-contact resolution already
  was; the test passes in 1.12 s.
- **Confirmed rows after a checkpoint import.** `ApprovalsTest` timed out in
  four of five concurrent JVM runs, and before the rebase too. The member held
  and acknowledged the decision's strong state, but its `decisions` table was
  empty: `finish_rebootstrap` reprojected before `install_strong` wrote the
  imported command rows, and no result remained to apply. `install_strong` now
  projects the confirmed table in its transaction, for checkpoint and recovery
  imports. `tests::confirmed_tables::a_checkpoint_import_projects_its_confirmed_rows`
  failed before the change and passes after it.
- **Rebase onto Phase 21.** `shared` is common Kotlin now: contact labels, the
  local device kind and registry command identities use portable file and
  SHA-256 functions; the contact dialog uses `pairing-ui`; driver actions moved
  to `ShoppingDrive`. Android secret names reach 255 bytes, since the temporary
  file no longer derives from the name.

## Budgets

Rust tiers pass without exceptions. `scripts/test-tiers` does not enforce Kotlin.
Alone, each in a fresh JVM that loads the native library, `RosterFreeTest` took
1.54–1.56 s (tables), 2.03–2.10 s (handover) and 2.57–2.59 s (scanned sharing)
(`rosterfree-isolated-22.log`). The last two miss the 2 s integration budget.
Earlier Household and issuer-recovery cases also exceed it inside the concurrent
suite, up to 11.8 s. No budget, tier or exception changed.

## Open observations

- Room's background invalidation refresh can open a connection while a session's
  authority changes and log an uncaught `SQLiteException: session authority
  changed`. It appears in some JVM runs and the iOS integration test, which
  pass, and already in pre-repair logs; `rocky-room` is unchanged. In an
  application process an uncaught coroutine exception may end it.
- `sdks/kotlin/inventory-qualification` still compiles its rejected fixtures
  against the deleted market `MarketCommands.kt`.
- Physical phones remain [phase 22](22-qualification.md).

## Earlier verification

The [node-reference repair](20-shopping-without-a-roster-privacy-repair.md) and
the [verification history](20-shopping-without-a-roster-history.md) keep the
earlier runs, blockers and emulator results.
