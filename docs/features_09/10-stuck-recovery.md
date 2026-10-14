# 10. Stuck recovery

Status: partial; retained-bundle collection implemented. Owner-signed cuts and
abandoned-history rollback are not implemented; see [evidence](10-stuck-recovery-evidence.md).
Owner: `rocky-coordinator` recovery operation; `rocky-secure` admission and
projection; `rocky-sdk` recovery flow. Depends on
[phase 9](09-coordinated-writes.md). Follow the
[execution contract](README.md#agent-execution-contract). Takes decision record 0073.

## Inputs and objective

Read [coordinated writes](09-coordinated-writes.md) and
[recovery and effects](../runtime/recovery-and-effects.md).

A domain is stuck when every node of its in-sync set is permanently cut off:
the coordinator knows the commit index, but no reachable trusted node holds
it. Rocky accepts this limitation. After this phase a stuck domain recovers
everything any device still holds, and continues past the rest only with the
domain owner's signature.

## Settled

- **Collect first.** A trusted node rebuilds missing committed entries from
  any reachable device, members included, using the position and hash each
  bundle carries, and checks them against the coordinator's hash window. A
  complete rebuild rejoins the set without any signature.
- **Owner-signed cut.** What no device holds needs a recovery record signed
  by the domain owner's key: the history continues after position `c` with
  hash `h`. The coordinator moves its head and commit index there.
- **Every device enforces the cut.** Frames of positions after `c` from the
  abandoned history are refused, and devices that applied them revert them.
  This is the only time members read log positions.
- **Beyond the window.** A gap older than the hash window cannot be checked
  and needs the owner-signed cut.

## Ordered slices

1. Decision record 0073 (with [removing strong commands](11-removing-strong-commands.md)):
   recovery records.
2. Rebuilding from reachable devices against the hash window.
3. The owner-signed cut at the coordinator and on every device.
4. Guides: coordinated writes, recovery and effects.

## Completion gate

- The last in-sync node is destroyed after publishing to a member; another
  trusted node rebuilds from the member and continues without a signature.
- Without any copy, appends stay refused until the owner signs a cut.
- A member returning later with an abandoned entry reverts it, and every
  device ends with the same rows.

## Remaining contract

The cut gate cannot be implemented by rewinding `secure_coordinated` alone.
Current bundle positions carry no history identity, so position reuse cannot
fence an abandoned frame. A verified checkpoint can already include the
abandoned suffix while its frame evidence is pruned; the current checkpoint
has no coordinated-log position/hash binding and rebuilding starts from that
same base. Recovery needs authenticated canonical-base replacement as well as
frame fencing. No cut API reports success before those contracts exist.

[Decision 0074](../decisions/0074-owner-signed-recovery-cuts.md) settles both:
the cut is a log entry after the replaced head, so positions are never reused;
checkpoints bind their coordinated history, and a tainted base is replaced by a
clean import. It also fixes the format changes, tests and landing order.
