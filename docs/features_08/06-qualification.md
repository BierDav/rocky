# 6. Qualification

Status: not started.
Owner: the shopping qualification and its evidence. Implements nothing.
Depends on [phase 4](04-remove-per-pass-sessions.md) and
[phase 5](05-status-kotlin-lifecycle.md). Follow the
[execution contract](README.md#agent-execution-contract).

## Objective

Show live links on real devices and networks, and measure what they cost.

## Required evidence

- A desktop and an Android phone on cellular, paired over iroh relays: an
  edit on either side appears on the other while both apps are in the
  foreground. Latency is recorded from commit to the observer firing, as
  median and 95th percentile over 50 edits each way.
- The phone cannot be dialed during the run, and edits from the desktop still arrive.
- Idle cost over one hour with an open link: messages, bytes and the
  phone's wake-ups, against the same hour before this roadmap.
- Background and foreground on the phone: no link while in the background,
  and catch-up within one reconcile after returning.
- A three-desktop mesh where every pair keeps a link. Killing one desktop
  closes its links within the liveness bound, and its restart reopens them.
- The committed revision of an idle pair does not move.

Record the evidence in `06-qualification-evidence.md` beside this brief.
