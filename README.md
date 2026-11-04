# Rocky

Rocky provides local-first SQLite storage, encrypted peer replication,
credentials, seal coordination and optional iroh networking.

Implementation follows [features_13](docs/features_13/README.md), including its
remaining prerequisites. [features_14](docs/features_14/README.md) records
additional application-facing requirements, not an implementation plan.
See [AGENTS.md](AGENTS.md) for contribution rules. Use Cargo manifests,
module declarations and tests to navigate the implementation.

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

The todo app shares ordered lists between trusted, full and partial
devices. Run several installations in one window, or one per device over
iroh; start, join and reset them in the window:

```sh
cargo run -p todo-desktop
cargo run -p todo-desktop -- --device --data-dir ./local/todo --lan
```

wasm32 (a browser, in a dedicated Worker) is a supported target of the same
crates; `./scripts/check --wasm` runs clippy for it and the tests in headless
Firefox. Status and limits are in features_14 R7.

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

Rust crates live in `crates/`, applications in `examples/` and performance
probes in `benchmarks/`.

Source-available under the [PolyForm Noncommercial License
1.0.0](LICENSE): read, run, modify and share Rocky for any noncommercial
purpose. Commercial use needs a separate paid license from the copyright
holder; ask [d.bieregger@gmail.com](mailto:d.bieregger@gmail.com).
Contributions are accepted under the inbound grant in [AGENTS.md](AGENTS.md).
