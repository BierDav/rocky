# Rocky

Rocky provides local-first SQLite storage, encrypted peer replication,
credentials, seal coordination and optional iroh networking.

Implementation follows [features_14](docs/features_14/README.md), building on
[features_13](docs/features_13/README.md). See [AGENTS.md](AGENTS.md) for contribution
rules. Use Cargo manifests, module declarations and tests to navigate the
implementation.

```sh
./scripts/setup
./scripts/lint
./scripts/check
```

For an edit, `./scripts/check --changed` validates the affected Rust packages
and their workspace consumers; shared or unknown inputs run the full workspace.
Use `./scripts/check --package rocky-sdk` to select a package explicitly, or
add `--unit` for just its unit tier. `./scripts/check --all` also runs e2e. These commands keep the workspace build and
filter test execution to avoid additional dependency variants. Cranelift's
separate build is opt-in with `ROCKY_CRANELIFT=1`.

The Rust application SDK is the `rocky-sdk` crate in `crates/rocky-sdk`. We only
provide the Rust-native API; developers who want to use it from Kotlin or other
languages need to write their own Rust wrapper/bindings.

Try it with:

```sh
cargo run -p rocky-sdk --example notes -- ./local/notes --create
cargo run -p rocky-sdk --example notes -- ./local/notes
```

A database's authority is an offline root key: `Configuration::create`
returns it once as a 24-word phrase (`Opened::root`), or
`create_with_root` takes a passkey-backed signer. Rocky never stores it.
The owner's identity is derived from that root. Identity signers are also
never stored: a new device joins as a single-use identity, or
`Configuration::join_linkable` returns its identity phrase once. Supply the
identity for later linking; opening and renewal use the node key alone.
The root grants and revokes trust per identity; those calls fail with
`publisher_required` without it. Every linked node of a trusted identity
can invite, renew, revoke ordinary identities and seal. Linking another node
needs the identity signer and matching codes on both devices, with no root
ceremony. Trusted observers retain `TrustedNodeLinked` and `TrustedNodeUnlinked`
alerts, including for their own identity. Trust never expires; an identity
raises `TrustedIdentityStale` on its trusted peers when none of its linked
nodes has been heard from within the configured threshold. Root revocation
keeps each linked node's seals and bookmark moves through the latest heads
the revoking node holds; unlink cuts that node's lane independently. Later
statements raise `TipStatementsPastCut`.
Every data key epoch is also sealed to the root's public key in records
that sync with its frames, so with every device lost the root and any
node's backup, a keyless custodian's too, restore the data that store
holds (`Configuration::recover`); the recovery revokes every trusted
grant older than its own, including ones its backup never knew. Of two
recoveries, the later one ends the earlier.

The todo app shares ordered lists between trusted, full and partial
devices. Run several installations in one window, or one per device over
iroh; start, join and reset them in the window:

```sh
cargo run -p todo-desktop
cargo run -p todo-desktop -- --device --data-dir ./local/todo --lan
```

wasm32 (a browser, in a dedicated Worker) is a supported target of the same
crates; `./scripts/check --wasm` runs clippy for it and the tests in headless
Firefox.

An application uses one `rocky-sdk` dependency for native nodes and the
browser; native-only networking and files are selected per target, not by
features. Building for `wasm32-unknown-unknown` compiles C (SQLite, ring), so
it needs a clang that targets wasm32 and an `llvm-ar` that indexes wasm
objects, which Apple's `ar` does not. In the application's
`.cargo/config.toml` (`rustup component add llvm-tools`; the path names your
host):

```toml
[env]
AR_wasm32_unknown_unknown = "<rustc --print sysroot>/lib/rustlib/<host>/bin/llvm-ar"
```

Rocky links its own SQLite build in the browser (`rocky-sqlite`, see
[vendor/README.md](vendor/README.md)); an application that uses rusqlite
itself depends on it with `default-features = false`, since rusqlite's default
`ffi-sqlite-wasm-rs` links another SQLite and the wasm32 link fails.

Rust crates live in `crates/`, applications in `examples/` and performance
probes in `benchmarks/`.

Source-available under the [PolyForm Noncommercial License
1.0.0](LICENSE): read, run, modify and share Rocky for any noncommercial
purpose. Commercial use needs a separate paid license from the copyright
holder; ask [d.bieregger@gmail.com](mailto:d.bieregger@gmail.com).
Contributions are accepted under the inbound grant in [AGENTS.md](AGENTS.md).
