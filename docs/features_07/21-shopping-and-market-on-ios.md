# 21. Shopping and market on iOS

Status: not started.
Owner: `examples/shoppinglist-app`, `examples/market-app`; the check
workflow. Depends on [phase 18](18-ios-packaging-keychain-lifecycle.md) and
[phase 19](19-market-example.md). Wave 2.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [the iOS scope](scope-ios.md), the two examples, the iOS guide written
by phase 18 and [shopping on devices](../runtime/shopping-devices.md).

Both applications keep their logic and screens in a `shared` Compose
Multiplatform module built for the JVM and Android. The market's `shared` is
portable by construction. Shopping's is not yet: it imports `java.*` in
`AppLink.kt`, `Coordination.kt`, `ShoppingNode.kt`, `ShoppingSession.kt` and
`ui/ApprovalsScreen.kt`, and its tests are JVM fragments. Build both for iOS
from the same sources and run them on the simulator and the iPhone.

## Settled

- One phase for both applications. Screens live in `shared`, so nothing is
  ported twice.
- The shopping procedure written here is install, offline write, background
  and resume. Pair, share and revoke on iOS are written once, by whichever
  of this phase and [phase 20](20-shopping-without-a-roster.md) lands
  second. If phase 20 has landed, this phase writes them on its schema; if
  not, this phase pairs by the existing flow only as far as its own steps
  need, and phase 20 writes the rest.
- Local discovery is reported as unavailable on iOS in this roadmap
  ([phase 15](15-discovery.md)). Rocky needs no local-network usage
  description, prompt or multicast entitlement, and the applications request
  none. iOS devices find peers by contacts and, once phase 15 lands, by the
  topic.
- The only permission the applications request is the camera.

## Ordered slices

1. `shared` of each example gains the iOS targets. Replace the `java.*`
   uses of shopping with portable calls of unchanged behaviour: paths as
   strings, URI encoding, random bytes and time formatting. Verify each
   dependency on the iOS targets (Metro, `qr-kit`, the icon set, the
   lifecycle view models, the hot-reload runtime); replace one only if it
   does not compile there. Platform code stays behind the existing
   expect/actual seams (`Codes`, `Permissions`, links).
2. An `iosApp` per example: the application entry, `KeychainSecretStore`,
   the lifecycle observer and the starter iroh transport.
3. Pairing on iOS: scanning a QR code with the camera and its permission;
   showing one; a paste fallback for the simulator, which has no camera.
4. A device procedure for iOS beside the Android one, under the split
   settled above. Each step is driven with `agent-device` and verified by a
   named expectation, not a screenshot.
5. A simulator check in CI on a macOS runner, added after
   [phase 1](01-test-speed.md) restructured the workflow. It builds both
   applications and runs the procedure steps that need no second device.

## Completion gate

- [Acceptance 5](scope-ios.md#acceptance) passes on the simulator paired
  with an Android emulator through a custody peer on the host: the shopping
  procedure of this phase, and a market reservation in each direction
  against three loopback voters.
- The same build installs and opens on the attached iPhone; pairing by
  camera works there.
- A list written offline on the simulator reaches the emulator after
  foregrounding, with no user action.
- No source file of either `shared` module is duplicated for iOS; the
  platform code added is listed with its reason.
- Neither `Info.plist` holds a local-network usage description or a Bonjour
  service list.
- The JVM and Android applications pass their existing checks, and the
  harness scenarios of both examples pass.
- [Shopping on devices](../runtime/shopping-devices.md) holds the iOS
  procedure and its first runs.

## Not in this phase

- The physical cross-platform pair across networks
  ([phase 22](22-qualification.md)).
- Local discovery on iOS, a Bonjour bridge or the multicast entitlement.
- A native SwiftUI screen, store distribution or signing automation.
- A new dependency-injection or QR library where the present one compiles.
- Schema or flow changes to shopping; those are phase 20's.
- Background sync.
