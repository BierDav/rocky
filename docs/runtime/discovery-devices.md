# Physical-device discovery gate

The dedicated probe uses Rocky's production discovery implementation and iroh
with relay and public lookup disabled. It announces under a known test topic,
resolves the other endpoint, verifies that endpoint through QUIC and echoes a
fresh 32-byte nonce. Each phone must report the other's endpoint ID and a local
route; success against a third peer does not satisfy the cross-device gate.

The probes have separate application IDs and do not touch shopping-list data.
The iOS app declares `_rocky._udp` and the usage description, and carries no
multicast entitlement. Android's probe holds a multicast lock in the foreground.
This gate establishes discovery and authenticated transport interoperability;
it does not establish database authorization, enrollment or row convergence.

## Build

Requires macOS/Xcode, the repository Rust toolchain, the pinned Android NDK,
a configured Apple development team and the Kotlin SDK's Gradle wrapper.
Both phones must be unlocked and connected to the same multicast-capable LAN.
USB is used for deployment and driving the apps, not the discovery connection.

```sh
sdks/native/qualification/discovery/build-devices \
  --team YOUR_TEAM --ios-device YOUR_IPHONE_UDID
```

The script stages generated UniFFI bindings, apps and build logs under
`local/discovery-probe`. `--bindgen` can select an existing Rocky UniFFI 0.32.2
generator. The gate's independent crate has its own locked dependencies and
is never included in packaged SDKs. Foreground/background transitions are
forwarded to the same production discovery capability as the SDK.

Artifacts:

- `local/discovery-probe/xcode/Build/Products/Debug-iphoneos/RockyDiscoveryProbe.app`
- `local/discovery-probe/android/app/build/outputs/apk/debug/app-debug.apk`

Install the dedicated apps, grant local-network access on iOS and launch both.
They display JSON status and write `result.json` (iOS Documents; Android files).
Require `stage=passed`, `local=true` and mutual `peer`/`endpoint` matches.
Background and foreground each app and require a new echo after resuming,
with the background local path `Off`. Resume the existing Android activity
with `am start --activity-single-top --activity-clear-top` rather than creating
a second activity; require the endpoint ID to remain unchanged. Allow up to
90 seconds for discovery and the 60-second announcement refresh. Record the
signed iOS entitlements and reject a build containing `com.apple.developer.networking.multicast`.

## Evidence

The implementation's unit tests cover topic, endpoint and port binding, version
refusal, expiry/future timestamps, malformed/oversized claims and candidate
budgets. The desktop transport test covers matching-topic discovery, outsider
silence and foreground/background status. The SDK's two-process
mDNS test separately verifies discovery and database convergence on the host.
Compilation or a simulator result alone is not a physical gate pass.

On 2026-10-12, the physical gate passed on an iPhone 15 running iOS 27.0 and
a Pixel 11 Pro running Android 17, on the same Wi-Fi LAN:

- Both apps discovered the other phone's endpoint and exchanged authenticated
  nonce echoes, with relay, public lookup and topic rendezvous disabled.
- The signed iPhone app had no multicast entitlement; local-network permission
  was granted through the ordinary iOS application permission flow.
- iPhone background/resume withdrew and restored the local path and produced
  a fresh echo with the same endpoint.
- Pixel background/resume released and reacquired the probe multicast lock,
  restored the local path and produced a fresh echo with the same endpoint.

JSON results and signed entitlements are in the workspace's ignored
`local/discovery-probe/` directory. They include `paired-evidence.json`,
`iphone-background.json`, `iphone-resumed.json`, `pixel-background.json`,
`pixel-resumed.json` and `iphone-entitlements.plist`.

This qualifies one LAN and the versions above. IPv6-only networks, denied
permissions, network changes, Android target-37 permission behavior and
physical-device database replication remain unqualified.

Verification also passed the discovery unit/integration tests, host SDK
convergence test, Android build, all four native iOS feature/target checks
and repository lint. The full repository check passed all 166 unit and 923
behavioral tests and Clippy, but failed its timing gate: the unrelated runtime
retention test `an_offline_replica_converges_or_rebootstraps_with_its_local_edits`
took 2.05 seconds against a 2.00-second limit in its isolated confirmation.
A subsequent isolated rerun passed in 0.320 seconds. The full check therefore
remains recorded as a timing-gate failure, with no functional test failures.
The runtime implementation and its budgets were not changed.
