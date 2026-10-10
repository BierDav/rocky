# Market: strong reservations on a coordination service

A Kotlin-only Compose application for JVM desktop, Android and iOS, with no Rust
source. Listings and memos are mergeable tables; `stock` and `holds` are
[command-controlled](../../docs/runtime/confirmed-tables.md) and change only
through the `Reserve` and `Restock` [typed commands](../../docs/runtime/typed-commands.md).
A [coordination service](../../docs/runtime/public-coordination.md) orders them,
so of two reservations for the last item one is accepted and the other gets a
typed rejection on every device. Its multi-process qualification is in
[market qualification](../../docs/runtime/market-qualification.md).

## Modules

Module names are unique in the workspace, so the market's carry a prefix.

| Module | Holds |
| --- | --- |
| `marketShared` | [Schema and commands](marketShared/src/MarketCommands.kt), [Room DAOs](marketShared/src/MarketDatabase.kt), the [market](marketShared/src/Market.kt), [composition](marketShared/src/MarketNode.kt) and the [one screen](marketShared/src/ui/MarketScreen.kt). No platform file API; native setup and pairing also use common screens |
| `marketDesktop` | The desktop application |
| `marketAndroid` | The Android application |
| `marketIos` | The iOS Compose host, Keychain, Room paths and lifecycle |
| `marketQualification` | The multi-process harness and the soak |

## The market

The device that creates the market is its owner: it keeps a 32-byte tenant key
in the platform's secret store, registers the domain and executes both
commands. Buyers join with `Market.Buyer` invitations: they read listings, write
memos and invoke `Reserve`. A command's row effects must lie within its
caller's rights, so buyers also hold insert and update on `stock` and `holds`;
SQL refuses their direct writes there as on every command-controlled table.

Each device publishes its command identity as a memo; the owner's upkeep admits
every published identity as an invoker with the next control revision. Each
control is stored before it is sent and retried with the same identity. Moving
the executors with `Market.control` fences the current executor; the new one
acquires the domain when it next runs a ticket of its own.

The screen lists offers with their confirmed stock, reserves one item and shows
the latest ticket: pending, outcome unknown while the same ticket is retried,
accepted or rejected with the typed values.

## Run

```sh
scripts/coordinator-local local/market-service
ROCKY_MARKET_BUNDLE=local/market-service/service/bundle.json ROCKY_MARKET_LOOPBACK=1 \
    ./kotlin run -m marketDesktop -- --create --invite local/market-invitation.txt
ROCKY_MARKET_BUNDLE=local/market-service/service/bundle.json ROCKY_MARKET_LOOPBACK=1 \
    ROCKY_MARKET_DATA=local/market-buyer ./kotlin run -m marketDesktop -- --join "$(cat local/market-invitation.txt)"
```

Data lives in `ROCKY_MARKET_DATA`, else `~/.rocky-market`. Without
`ROCKY_MARKET_LOOPBACK`, devices use iroh with relays and discovery.

The Android app is started over adb with the service's bundle; see
[the emulator procedure](../../docs/runtime/market-qualification.md#android-emulator).

## Verify

```sh
./examples/market-app/check     # commands, Room DAOs, schema; compiles the apps
./examples/market-app/qualify   # the five scenarios against three voters
./examples/market-app/soak      # 4,096 commands; outside the default checks
```

## iOS

Build both shared target architectures and the simulator apps with
`./examples/shoppinglist-app/check-ios`. `iosApp/marketIos` stores Rocky and
Room under Application Support and uses the device-only Keychain for runtime
and tenant secrets. The common `MarketApp` accepts a service bundle and an
invitation; an issuer shows a buyer QR code. A simulator pastes the invitation;
a device scans with camera permission. If the invitation includes addresses
from another network, enter the issuer’s reachable `host:port` in **Direct pairing
address (optional)**. This selects one iroh IP route while retaining relay
fallback, the endpoint identity and all enrollment credentials. Android and desktop use the
same pairing screen. EasyQRScan scans on mobile; QRose displays pairing codes.
The iOS hosts disable mDNS; pasted or scanned invitations
work without discovery. Foregrounding resumes SDK sync and common command upkeep; backgrounding
cancels both.

On iOS, **Reset market** returns to setup after confirmation, closing the open
session, removing this device's market files and selecting a fresh Keychain
namespace. Use it if you created a market when you meant to join; it leaves
other devices and the Shopping app untouched. Opening progress and errors stay
visible below the setup fields. If an issuer replaces an invitation after a
failed join, reset this device before scanning the new invitation; retries of
the same invitation retain the existing pending join.

For local simulator checks, run `scripts/coordinator-local local/market-service`
and paste `local/market-service/service/bundle.json`. For an Android emulator
pair, use `./examples/market-app/emulator-service` so the certified voter
addresses include the host's Android loopback alias. Create a market on one,
join the other with **Pair buyer**, list and restock one item, and reserve from
the buyer. Expect **Accepted: 0 left**. Repeat with the other platform as owner
and executor. Each direction requires its own fresh market. The iPhone needs a
service and peer route reachable from its network, and a signed installation.
For an emulator listener, `python3 examples/market-app/emulator_udp.py --serial
emulator-5562 --port PORT` prints a symmetric localhost UDP address for the
other device's manual field. Keep it running while paired devices exchange
data; Ctrl-C removes its own forwarding rule. It forwards the real protocol
bytes, including replies from the emulator's different outbound NAT port.
The [Phase 21 evidence](../../docs/features_07/21-ios-applications-evidence.md)
separates simulator results from the required device and cross-platform gates.
