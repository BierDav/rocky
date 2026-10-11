# Links only to useful peers

Status: idea; not planned.

[Live links](../features_08/README.md) keep one open link to every peer a
device syncs with. That fits a household of a few devices. In a larger mesh
every device would hold a link to every other: the link count grows with the
square of the mesh, each phone keeps pings to peers it does not need, and the
link budget refuses peers at random once it is full.

A device should keep links only to peers that are useful to it, and reach
the rest through them:

- **Holdings.** A peer that holds none of the scopes this device holds adds
  nothing directly. Prefer peers whose proven holdings cover this device's
  scopes; a few peers that together cover every scope are enough.
- **Reach.** Prefer always-on and custody peers, and peers on the local
  network, over peers that are rarely online or only reachable by relay.
- **Authority.** Keep at least one issuer within reach for renewal and
  revocations, and an executor for strong commands when one is known.
- **Redundancy.** Keep two covering peers per scope where possible, so losing
  one link does not delay changes until the next dial.
- **Fan-out.** A peer that receives a pushed frame forwards it on its own links,
  so a change crosses the mesh in a few hops instead of reaching every
  device directly. Frames are already signed and admitted per receiver; the
  open questions are duplicate suppression and the bound on forwarding.

Partner ranking from [contacts and partner choice](../old/features_07/14-contacts-and-partner-choice.md)
already scores peers; a link set could be its top entries under these rules,
re-evaluated when holdings or reachability change, not on every pass.
