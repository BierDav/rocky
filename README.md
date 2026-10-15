# Rocky DB

Rocky supplies local-first SQLite storage, encrypted replication, credentials,
coordinated writes and optional iroh networking. The metadata coordinator orders
ordinary row bundles without holding application data or business rules.
Applications define their schemas and transitions; see
[coordinated writes](docs/runtime/coordinated-writes.md).

The accepted [architecture](docs/proposals/architecture-proposal-v4.md) and
[current roadmap](docs/features_09/README.md) govern implementation. Phase 11
removes the earlier strong-command model; its [evidence](docs/features_09/11-removing-strong-commands-evidence.md)
records the incomplete native/Kotlin, example and workspace verification gates.
The current change is not a qualified release. No live public service or
universal language/ORM support is claimed.

The managed runtime owns transactions, row capture, operation history, outboxes
and committed observation. Applications can use SQL and supported ORM adapters;
network ownership stays optional. Read [SQL sessions](docs/runtime/sql-sessions.md),
[secure sessions](docs/runtime/secure-sessions.md),
[database bindings](docs/runtime/database-bindings.md) and
[queries and observation](docs/runtime/queries-and-observation.md).

Schemas use names as identities, adopt publisher-signed generations and drop
removed tables and fields after issuer confirmation. Older applications continue
using unchanged fields; incompatible local work becomes held changes. See
[generations](docs/runtime/generations.md), [held changes](docs/runtime/held-changes.md)
and [the module map](docs/modules.md).

[Room integration](docs/runtime/room-integration.md),
[SDK packaging](docs/runtime/sdk-packaging.md), [iOS](docs/runtime/ios.md),
[scaling evidence](docs/runtime/scaling-limits.md) and
[coordinator evidence](docs/runtime/coordinator-evidence.md) record their own
qualification boundaries. Passing core package tests does not qualify a desktop,
mobile, packaged SDK or distributed application.

## Getting started

Install Rust through rustup, Jujutsu (`jj`) and ripgrep (`rg`). The repository
toolchain file selects the Rust release and components automatically.

```sh
./scripts/setup   # pinned lint tools, jj fix/push configuration
./scripts/lint
./scripts/check
```

We use **Jujutsu exclusively** for version control, including remote operations
through `jj git`. Do not use the Git CLI in this repository.

Desktop demos still need the Phase 11 conversion recorded in the evidence above.

## Where to look

| Area | Purpose |
| --- | --- |
| `crates/rocky-core` | Shared protocol types and portable schema definitions |
| `crates/rocky-storage` | SQLite access, row capture, statement analysis, snapshot cursors and table events shared by the runtimes |
| `crates/rocky-runtime` | Trusted transactions, replication and observation |
| `crates/rocky-credentials` | Credential verification, issuance, grant profiles, key providers and enrollment messages |
| `crates/rocky-secure` | Capability enforcement, encrypted replicas, key epochs and live link sessions |
| `crates/rocky-iroh` | Optional iroh handlers for an application's endpoint, and the starter's own |
| `crates/rocky-sdk` / `sdks/native` | Generic native SDK lifecycle and UniFFI ABI |
| `crates/rocky-coordinator` | Metadata log ordering and trusted membership |
| `examples/directory-app` | Identity registration, scoped delegation and deletion disposition |
| `examples/todo-app` | Todo schema and merge rules |
| `examples/inventory-app` | Manifest-bound draft notes |
| `examples/todo-desktop` | Desktop composition and development controls |
| `sdks/kotlin/rocky-core` / `rocky-room` | ORM-neutral Kotlin APIs and Room projection/action adapter |
| `examples/shoppinglist-app` | Desktop library walkthrough, pairing and Room migration qualification |
| `benchmarks/engine-comparison` | Separate engine qualification workspace |
| `benchmarks/engine-scale` | Separate workspace comparing engines at target size |
| `benchmarks/runtime-scaling` | Scaling probes and timing qualification for the runtime |

Read [module boundaries](docs/modules.md) for ownership and dependencies.
Use [the documentation index](docs/README.md) to select a focused guide.

- [Development workflow](docs/development.md)
- [Commit messages](docs/commits.md) and [releasing](docs/releasing.md)
- [Agent instructions](AGENTS.md)
- [Documentation conventions](docs/documentation.md)
- [Implemented operation semantics](docs/applications/todo/operation-semantics.md)
- [Current implementation roadmap](docs/features_09/README.md)
- [Current architecture proposal](docs/proposals/architecture-proposal-v4.md)
- [Engine decision](docs/runtime/write-concurrency.md#engine)
