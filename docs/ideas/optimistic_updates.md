# Optimistic updates for strong commands

Status: idea; not scheduled.

## Background

Mergeable writes need no optimistic layer: with the SQLite driver, a DAO
write commits locally at once and is never rolled back.

A strong command is different. Its ticket is durable from `submit`, but the
outcome arrives only after the executor and the coordinator confirmed it. Until
then a screen shows nothing, or shows the command as pending.

## Idea

A prediction is safe when it is derived and never stored:

- A handler is a pure function of its arguments and the confirmed rows, so the
  SDK can run it locally as a preview over the confirmed rows plus the earlier
  unresolved tickets of this device.
- The preview is recomputed on every confirmed change.
- A prediction is never persisted, synced, readable by SQL as confirmed or
  allowed to trigger effects. `outcome_unknown` renders as pending, never as
  failed.

Surface: a `tickets()` Flow, `preview(command, arguments)` and an opt-in
`overlay(table)` Flow that tags each row confirmed or predicted. It stays out of
Room DAOs.

## Why it waits

It is an SDK convenience with no format impact and no effect on the runtime,
so it can follow once strong commands, keyed handler reads and bounded
history are in place.

## Open

- Whether a preview may run a handler that is hosted outside the process, or
  only an in-process one.
- How a preview behaves while the device lacks a complete confirmed state
  (`Incomplete`): render nothing, or render over what is held.
