# rocky-sqlite

Rocky's SQLite: the SQLite 3.53.2 build that native Rocky bundles through
libsqlite3-sys, compiled for `wasm32-unknown-unknown` with the same flags, and
a preupdate hook for rusqlite connections on every target. A fork of
[sqlite-wasm-rs](https://github.com/Spxg/sqlite-wasm-rs) 0.5.5 under its MIT
license; see Rocky's `vendor/README.md`.
