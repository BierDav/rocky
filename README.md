# Rocky DB

Rocky provides local-first SQLite storage, encrypted peer replication,
credentials, coordinated writes and optional iroh networking.

Implementation follows [features_11](docs/features_11/README.md).
See [AGENTS.md](AGENTS.md) for contribution rules. Use Cargo manifests,
module declarations and tests to navigate the implementation.

```sh
./scripts/setup
./scripts/lint
./scripts/check
```

Rust crates live in `crates/`, language bindings in `sdks/`, applications
in `examples/` and performance probes in `benchmarks/`.

Source-available under the [PolyForm Noncommercial License
1.0.0](LICENSE): read, run, modify and share Rocky for any noncommercial
purpose. Commercial use needs a separate paid license from the copyright
holder. Contributions are accepted under the inbound grant in
[AGENTS.md](AGENTS.md).
