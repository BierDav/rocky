# 9. Qualification

Status: planned. Owner: whoever runs the devices. Code owners of phases
1–8 fix regressions found here. Depends on [phases 1, 2 and 8](README.md#phases).
Effort: one week. Carried over from features_12 phase 6, with intent
gates added. Follow the [execution contract](README.md#agent-execution-contract).

## Objective

Show on real devices that writes stay fast, replicas converge with
identical drop maps, and features_11's sync results didn't regress.

## Settled

- **Builds.** One integrated build from the roadmap's final change and
  one from the features_13 start commit, same toolchain and release
  profile. Record SHA-256 identities for the macOS library, the physical
  iOS static library, the Android arm64 library and each signed app.
- **Devices.** Two desktops, the physical iPhone 15 and the Pixel from
  features_11, on one LAN, direct paths, relay disabled. Record network
  snapshots.
- **Pairing.** Each measurement runs on both builds back to back on the
  same devices, data and order.
- **Unavailable devices.** Their rows are marked "not verified", never
  filled from desktop numbers.

## Ordered slices

1. Phone L0–L4 write probe on both builds, with the per-site holder
   table, reactor polls and samples overlapping quiet-gate steps.
2. Sync non-regression rows from the
   [README](README.md#sync-must-not-regress).
3. Shopping app: three devices editing one list concurrently, including
   concurrent increments of one quantity and a constraint that drops one
   of two conflicting edits, while one phone bootstraps the isolated
   100k load. Check every device converges to the same rows and drop map
   at the next seal, and that the dropping device shows the notification.
   Record each transition or rewind that overlaps an edit; this evaluates
   [phase 10](10-bounded-transitions.md)'s trigger.
4. Offline: one device edits for a period spanning a routine seal by
   another, reconnects, and its late transactions replay in the successor
   tip. Record drops and their reasons.
5. Offline restart: kill the editing app right after a buffered commit
   returns, reopen offline, verify the write is present. A write killed
   before `commit()` returns is absent or complete, never partial.
6. No owner online: let the open tip grow to 4× the routine seal
   constant; record rewind costs and confirm convergence.
7. Evidence: a goals table with baseline, result and pass/fail per row,
   build identities, raw result paths and each phase's open items. Set
   the README status.

## Completion gate

- Every row of the README goals and sync non-regression tables has a
  measured result on its named device class, or "not verified" with the
  reason.
- Every convergence check in slices 3, 4 and 6 holds.
- Misses are listed with their profile and the holder or step
  responsible, and phase 10's trigger is evaluated.
- `./scripts/check`, the Kotlin check and the shopping app check pass on
  the integrated build.

## Not in scope

New optimizations. A miss goes back to the owning phase or to a later
roadmap, not fixed ad hoc here.
