# 6. Qualification

Status: planned. Owner: whoever runs the devices. The code owners of
phases 2–5 fix regressions found here. Depends on
[phases 2, 3, 4 and 5](README.md#phases). Effort: three to five days.
Follow the [execution contract](README.md#agent-execution-contract).

## Objective

Show on real devices that the sprint's goals hold together, and that
features_11's sync results didn't regress. Earlier phases measured mostly
on desktop. This phase measures the integrated build on phones, with the
shopping app and the isolated 100k load.

## Settled

- **Builds.** One integrated build from the sprint's final change, and
  one from the features_12 start commit. Both use the same toolchain and
  release profile. Record SHA-256 identities for the macOS library, the
  physical iOS static library, the Android arm64 library and each signed
  app executable, as the features_11 phase 9 evidence did.
- **Devices.** Two desktops, the physical iPhone 15 and the Pixel from
  features_11, on the same LAN, with direct paths and relay disabled.
  Record the network snapshots.
- **Pairing.** Each measurement runs on both builds back to back on the
  same devices, with the same data and the same order. Report both.
- **Unavailable devices.** If a device is unavailable, its rows are
  marked "not verified". They are never filled from desktop numbers.

## Ordered slices

1. Phone L0–L4 write probe on both builds (iPhone and Pixel), with the
   per-site holder table.
2. Sync non-regression rows from the
   [README](README.md#sync-must-not-regress):
   - phone 100k bootstrap
   - one-row sync p95 (20 alternating updates)
   - empty syncs
   - member and issuer file sizes
3. Shopping app: three devices editing one list while one phone
   bootstraps the isolated 100k load. Record the probe's percentiles on
   the editing phone, and check that every device converges.
4. Offline restart: kill the editing app right after a buffered commit
   returns, reopen it offline, and verify the write is present.
   Separately, confirm that a write killed before `commit()` returns is
   absent or complete, never partial.
5. Evidence: a goals table with baseline, result and pass/fail per row;
   build identities; raw result paths; and the status of each phase's
   open items. Set the README status.

## Completion gate

- Every row of the README goals table and the sync non-regression table
  has a measured result on its named device class, or is marked not
  verified with the reason.
- Rows that miss their target are listed with their profile and the
  holder or step responsible. The README's "Revisit deferred authoring"
  criteria are evaluated against them, and the outcome is stated.
- `./scripts/check`, the Kotlin check and the shopping app check pass on
  the integrated build.

## Not in scope

New optimizations. A miss found here goes back to the owning phase, or to
a features_13 brief. It is not fixed ad hoc during qualification.
