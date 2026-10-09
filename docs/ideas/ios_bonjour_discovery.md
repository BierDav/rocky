# Local discovery on iOS through Bonjour

Status: idea, recorded on 2026-10-10. Not scheduled. The current roadmap finds
local peers with mDNS alone ([discovery](../features_07/15-discovery.md)).

## Background

Local discovery uses one mechanism on every platform: an mDNS address lookup
for iroh, which opens its own multicast socket. On iOS that socket needs the
multicast entitlement (`com.apple.developer.networking.multicast`), which
Apple grants per developer account on request, and the local-network prompt.
The simulator needs neither. The entitlement was requested on 2026-10-10.

Apple's own APIs find services without that entitlement. `NWBrowser` and
`NWListener` from the Network framework, and the older `NetService`, browse
and advertise Bonjour services through the system's mDNS responder. They need
only the local-network usage description and the service types listed under
`NSBonjourServices`.

## Idea

A second local-discovery path for iOS that announces and browses through the
system instead of a raw socket:

- The application, or the iOS part of `rocky-core`, advertises a Bonjour
  service whose name and TXT record derive from the discovery topic, and
  browses for the same type.
- What it finds enters the contact list as a local hint, exactly as an mDNS
  result does. The session still authenticates whoever answers.
- The transport's discovery capability stays one contract; the platform
  supplies the implementation, as it does for the secret store.

It would let a physical iPhone find peers on a LAN in a build that has no
multicast entitlement, including a store build while that entitlement is
pending or refused.

## Why it waits

- One mechanism is simpler to qualify. The simulator covers iOS for the
  roadmap, and an iPhone still finds peers by contacts and by the topic.
- A bridge is a second implementation of discovery, in Swift or through
  Kotlin/Native platform interop, driven by callbacks from the system on its
  own queues. That is the hard part of the binding again.
- The two paths must interoperate: an Android device announcing through the
  mDNS crate and an iPhone browsing through Bonjour must agree on the service
  type, the instance name and the TXT layout. Neither is specified yet.
- Once the entitlement is granted the mDNS crate may be enough on devices,
  and this idea may not be needed at all.

## Open

- Does the mDNS crate announce a standard DNS-SD service that `NWBrowser`
  can find, or a layout only its own peers read?
- `NSBonjourServices` lists service types statically in `Info.plist`. The
  type therefore cannot derive from the topic; only the instance name or the
  TXT record can. What does a LAN observer then learn?
- Does a hybrid device run both paths, and which one wins a duplicate?
- Where does the code live so that an application-owned endpoint keeps its
  own discovery configuration?
- What does a declined local-network prompt look like to each API, so the
  path is reported as unavailable instead of silently finding nothing?
