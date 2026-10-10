# Market: strong reservations on a coordination service

A Kotlin-only Compose application for JVM desktop and Android, with no Rust
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
| `marketShared` | [Schema and commands](marketShared/src/MarketCommands.kt), [Room DAOs](marketShared/src/MarketDatabase.kt), the [market](marketShared/src/Market.kt), [composition](marketShared/src/MarketNode.kt) and the [one screen](marketShared/src/ui/MarketScreen.kt). No platform file API, so iOS adds a target only |
| `marketDesktop` | The desktop application |
| `marketAndroid` | The Android application |
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
