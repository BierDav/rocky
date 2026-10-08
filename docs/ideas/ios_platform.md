# iOS platform

Status: idea; not scheduled.

## Background

Android is the first mobile platform: native packaging, lifecycle handling and
a Keystore-backed `SecretStore`. Only JVM desktop is configured today.

## Idea

The same packaged native library on iOS, with the platform pieces Android gets:

- Packaging as an XCFramework, with the C ABI the Kotlin bindings use.
- A Keychain-backed `SecretStore`; keys are plaintext files today.
- Lifecycle handling: background execution limits end sync and effect
  dispatch, so a resume must continue them from durable state.
- A host for the application protocol on an application-owned endpoint, as
  [SDK transports](../runtime/sdk-transports.md) allow on the JVM.

## Why it waits

A second platform doubles the qualification surface before the first real
application exists. Android alone already carries the packaging, lifecycle and
secret-store work; iOS repeats those with different platform rules and adds
the Swift surface.

## Open

- Swift bindings generated from the same contract as the Kotlin ones, or a
  thin hand-written layer over the C ABI.
- Whether Room-style integration has an iOS counterpart, or the SQLite driver
  is exposed directly.
