# Kotlin SDK and packages

The Kotlin APIs call the runtime in-process through UniFFI. Schemas, typed
command handlers and transports live in application Kotlin; consumers link
packaged artifacts and need no Rust project, Cargo, Python or native
executable. Contributor generation still uses those tools. The handles are
described in [composition](sdk-composition.md); [Android](android.md) adds
lifecycle.

## Compose services

```kotlin
val db = RockyDatabase.local(directory, schema, secrets = mySecretStore)
val session = db.openSession()
val authority = session.credentials.status()
val reads = session.observe(schema.table("items").query())
val progress = session.observeStatus()
val network = db.attach(PeerTransport.Endpoint(myEndpoint))
network.close() // database and application endpoint continue
session.close() // other sessions continue
// db.close() ends its sessions and network, then flushes/closes its file
```

`local`/`open` default to no network; `create`/`join`/`resume` are starter
helpers over the same handles, attaching iroh and automatic sync by default.
`RockyDatabase` is itself its default session.

`RockySession` implements `RuntimePort`: `Row` reads and `write { }` SQL
transactions without an ORM. Schemas come from `schema { }` or `schemaFromRoom`;
an insert takes its `id` from `nextId(table)` using the number in the node
credential. `session.commands` is the `CommandPort`; only execution and
delivery need the coordinator and optional handlers passed as `CommandRuntime`.
`DevelopmentCoordinator` is an in-memory process fixture;
`ServiceCoordinator(trustBundle)` pins a provider or self-hosted service
reached over the database's iroh transport. `session.service(tenantKey)`
returns the owner's `ServiceOwner` (`register`, `control`, `activate`,
`usage`); `CommandIdentity.encoded`/`decode` carry device identities to it
([starter guide](getting-started.md)).

`Credentials.prepare` returns public keys for an external issuer;
`RockyDatabase.install` accepts the supplied credential/reader-key bundle,
verified natively before installation. Enrollment, renewal, revocation and
logout use `session.credentials` ([enrollment](sdk-enrollment.md)).

## Batches, cancellation and reconciliation

Queries return bounded tagged rows in one native call. `query(..., cursor = next)`
continues a page; `maxRows` is 1–1,024 and pages also cap encoded bytes.
Continuations are `page.v1:<offset>:<commitment>`, a session-private random
commitment over manifest, authority epoch/expiry, query and offset. They
retain no cursor; malformed, cross-session, cross-query and stale tokens
refuse. Concurrent edits may shift pages: restart a scan when revisions
change. After renewal, fresh calls use current authority and old
continuations refuse. Errors carry stable `code`, `retryable` and a
diagnostic `message`.

`observeStatus()` is a cold Flow adapting native `statusAfter(token)` waits.
It exposes independent authority, data, network and coordination facts through
generated native types. Expiry and runtime transitions wake observers without
health polling. See [observable status](../proposals/observable-status.md).

Foreign cancellation abandons the wait; admitted writes keep running. Persist
the `handle` passed to `write` before writing, then `session.reconcile(handle)`
after interruption: false means no known local commit. A different transaction
under a committed handle is refused, so write again only when it did not
commit. For commands, persist and rerun the same `ticket(command, handle)`.
Confirmed or unknown outcomes are never converted into fresh-ID retries.

## Build and consume a package

`sdks/kotlin/gradlew -p sdks/kotlin :qualification:packaged-jvm:packageSdk` generates UniFFI Kotlin from the
ABI, compiles the ORM-neutral SDK and a Kotlin-only consumer, and writes
`sdks/kotlin/qualification/packaged-jvm/build/packaged-sdk`: `lib/rocky-core-jvm.jar` (bindings plus JNA OS/CPU
resources), the JNA, coroutines, serialization and Kotlin standard-library
jars, `Consumer.kt`, `consumer.jar`, a launcher and a SHA-256 inventory.
[Releases](../releasing.md) publish the jar to GitHub Packages as
`rocky:rocky-core-jvm` and the directory as a release asset. `sdks/kotlin/gradlew -p sdks/kotlin :qualification:packaged-jvm:qualifyPackage` copies
only artifacts outside the repository and runs with an empty environment and
only Java on `PATH`. It covers local sessions, status Flow and cancellation,
handle reconciliation, Kotlin handlers, durable tickets, network ownership,
fresh restore and additive upgrade with reopen, on a development coordinator.
The issuer backup is restored with an issuer invitation, preserving access to
its registry projection. The upgraded schema is reopened again to check its
nullable field, durable ticket and original mutation handle.

| Artifact | Default iroh | Without iroh |
| --- | --- | --- |
| Rust SDK | Local database, owned or borrowed networks, composed/service coordinator | Local database without a network, composed coordinator |
| Native ABI | Databases, sessions, callbacks, endpoint handles, service bundles | Databases and sessions without a network; network, endpoint and service configuration refuse |
| Kotlin JVM package | Qualified | ABI compile gate only |
| Android AAR (`arm64-v8a`, `x86_64`, API 26+) | See [Android](android.md#qualification) | No claim |

| Target | Evidence |
| --- | --- |
| macOS arm64 (`darwin-aarch64`) | Clean packaged consumer, JDK 25 |
| Linux x86_64 (`linux-x86-64`) | Clean packaged consumer, Debian 12, OpenJDK 17, under emulation |
| Windows x86_64 | Best-effort build, no claim |
| iOS arm64 device/simulator | [Kotlin/Native binding qualification](kotlin-native-binding.md); [packaged artifacts](ios.md#package-and-consume) |
| web | Unqualified |

The iOS binding uses cinterop over UniFFI’s generated header and the static
bundled-SQLite runtime; the JVM and Android retain JNA. Common openers take
`String` paths, with `File` overloads on JVM/Android. Synchronous callbacks
arrive on Rust threads and must be thread-safe; foreign async traits use
Kotlin’s default dispatcher. Explicit closure releases owned handles and
application endpoints remain borrowed. See the [binding guide](kotlin-native-binding.md#threads-cancellation-and-ownership)
for cancellation, ownership and measured limits. [iOS](ios.md) describes consumer artifacts, secrets and lifecycle.

JVM bytecode targets 17. The JVM package carries the locked development
native profile; publication, optimized artifacts and signing/notarization are
not done. The partitioned shopping schema is refused by the current constraint
boundary ([audit](shopping-qualification.md)).

## Another language

Another binding implements the same version-1 scoped JSON call/error/batch
contract ([SDK contract](sdk-contract.md)), opaque session ownership, stable
intent persistence and committed observation semantics. Generate bindings with
the pinned UniFFI 0.32.2 from the packaged ABI (`uniffi.toml` is the Kotlin
example). Supply secret-store and stream callbacks and deterministic handlers
through foreign interfaces. Callbacks receive immutable input and return
bounded decisions; they never own a storage transaction or perform external
effects. Foreign exceptions, overruns and crashes map to named failure classes
and timeouts, never a panic or stuck domain worker. Qualify cancellation,
scalar precision, exception mapping, callback deadlines and borrowed-resource
lifetime before release.

## Design rationale

- In-process UniFFI bindings are the one consumer adapter. A sidecar process
  over a local runtime protocol was rejected: it doubles the protocol surface,
  cannot run on iOS and makes sharing one iroh endpoint with application
  protocols awkward. A hand-written C ABI was rejected because UniFFI generates
  it together with typed bindings. A future sidecar would be an additional
  adapter carrying the same bytes and host contract.
- One UniFFI version is pinned across runtime and bindings; a mismatch is a
  load error. Binding object graphs are never stored or sent.
- The runtime cannot prevent foreign handler I/O: handler determinism is
  trusted, and handler identity is declared and installed, not proven.

## Contributor SHA-2 build prerequisite

Little-endian AArch64 Apple, Linux and Android builds enable the pinned SHA2
0.10.9 upstream `asm` feature. Its transitive sha2-asm 0.6.4 uses `cc` to
assemble target code, so native contributors need the existing Xcode SDK,
Android NDK or Linux cross-toolchain with its assembler. Other target families
keep the existing backend. [Backend evidence](../features_09/17-sha2-backend-evidence.md)
distinguishes link checks from runtime coverage.

The assembler is a contributor prerequisite. Packaged SDK consumers continue
to use prebuilt native resources with their declared runtime prerequisites;
they do not need Rust, a C compiler or an assembler. Cross-built resources still
need the ordinary final-source packaging qualification before release.
