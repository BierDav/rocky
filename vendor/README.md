# Pinned Darwin network declarations

`netdev` 0.46.3 and `netwatch` 0.19.3 are exact copies of their published
crates, including their licenses. Only their libc dependency entries change:
they resolve `rocky-apple-libc` instead of `libc`. Both workspace manifests
patch those exact crates; other netdev/netwatch versions remain upstream.

`apple-libc` reexports pinned upstream libc 0.2.190. On iOS it additionally
exports the Darwin routing constants and IPv6 ioctl types that libc declares
only for macOS. Its declarations are copied from libc 0.2.190's Darwin source,
under the included MIT/Apache licenses. The Darwin XNU network ABI is shared
by these targets: `in6_ifreq` has size 288, alignment 8 and its union at offset
16. Compile-time assertions check that layout on each iOS target.

No network implementation, route watcher, interface enumeration, endpoint
admission or feature changes. The overlay has no authored
unsafe code. Vendored upstream dependency implementations retain their
original unsafe code and licenses, just as registry dependencies do.

This narrow arrangement resolves the missing `in6_ifreq` used by netdev and
the route constants used by netwatch, while preserving the locked iroh 1.3.0
path.

# The browser's SQLite

`rocky-sqlite` is Rocky's fork of `sqlite-wasm-rs` 0.5.5 (MIT, by Spxg), the
wasm32 SQLite rusqlite 0.40 links by default. It publishes under Rocky's name
because a `[patch]` does not reach applications. Its `sqlite3/` amalgamation
is libsqlite3-sys 0.38.2's, the SQLite 3.53.2 that native Rocky bundles, and
`build.rs` compiles it with libsqlite3-sys's bundled flags. Replay results
depend on the engine, so the targets must not differ. Only what a browser
cannot have differs: `SQLITE_OS_OTHER` (the VFS registers at runtime),
`SQLITE_THREADSAFE=0`, `SQLITE_OMIT_LOAD_EXTENSION` and `SQLITE_TEMP_STORE=3`.
Upstream's extra features (math functions, `sqlite_offset`, sessions, the
bytecode and stmt tables) are off, as they are natively. The sqlite3mc variant
and upstream's bindings are removed.

In a browser rusqlite runs without its `ffi-sqlite-wasm-rs` feature, so
`rusqlite::ffi` is libsqlite3-sys with its prebuilt 3.53.2 bindings. Without
`bundled`, libsqlite3-sys links `-lsqlite3` (pkg-config refuses to cross
compile), and `build.rs` archives the amalgamation and libc shim as
`libsqlite3.a` on the linker's search path. An application needs no patch or
variable for it. If an application's own rusqlite keeps its default features,
upstream's SQLite returns and the link fails with duplicate symbols.
rusqlite's `preupdate_hook` feature would run bindgen against a system
`sqlite3.h`, which a wasm32 build lacks, so `rocky-sqlite` also provides the
preupdate hook on every target; natively it enables libsqlite3-sys's
`preupdate_hook`. `build.rs` archives with the toolchain's `llvm-ar` (rustup
component `llvm-tools`), because a host `ar` such as Apple's writes no symbol
index for wasm objects. `rocky-storage`'s `every_target_links_the_same_sqlite`
checks the version, source id and compile options on every target.

`rocky-sqlite-opfs` is Rocky's fork of `sqlite-wasm-vfs` 0.2.0, the OPFS
sync-access-handle pool (`sahpool`). It is a workspace member and publishes
under Rocky's name, because `rocky-platform` depends on it; it keeps upstream's
MIT license and edition, and the workspace lints do not apply to its `unsafe`
C-ABI code. Rocky adds `src/locks.rs`: SQLite's file locks and a heap
wal-index shared by the connections one worker opens, behind version 2 io
methods (`xLock`, `xUnlock`, `xCheckReservedLock`, `xShm*`). Upstream's locks
are no-ops and it has no shared memory, so WAL was impossible and several
connections to a file could see each other's unfinished pages. OPFS access
handles already exclude other workers and tabs; the lock state is plain safe
Rust with unit tests, and the callbacks only translate pointers and codes.
`OpfsSAHPoolUtil` gains `write`, `flush`, `rename` and `size` for
`rocky_platform::fs`. The IndexedDB VFS is removed. `rocky-storage`'s
`files::tests` run the same WAL and locking test natively and in Firefox.
