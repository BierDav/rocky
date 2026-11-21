# Mesh benchmark

An independent Cargo workspace that measures Rocky between real hosts: how
fast a small change reaches every peer, what a migration of every row costs,
how a peer returning with offline changes affects the mesh, how fast a fresh
device bootstraps over a direct path and over a relay, and what Rocky adds on
top of plain SQLite on one machine. Results are in [RESULTS.md](RESULTS.md).

## Layout

- `src/agent.rs` is a Rocky node driven by JSON lines on standard input; it
  answers on standard output and reports every probe row it observes.
- `src/net.rs` is the iroh connector. `default` is `rocky_iroh::Options`
  unchanged, `bind:<ip>:<port>` binds one interface so tunnels such as
  WireGuard overlays are not used, and `relay` has no IP transport at all, so
  every byte goes through the relay.
- `src/local.rs` runs one workload against plain SQLite (WAL, `synchronous`
  NORMAL or FULL) and against Rocky (`Durability::Buffered` or `Flushed`).
- `mesh.py` starts agents locally or over `ssh` and estimates each host's
  clock offset from the lowest-latency round trip of 15 samples.
- `drive.py` holds the scenarios and the host table; `analyze.py` prints the
  tables in RESULTS.md from a results directory.

## Workload

`items(grp, title, body, n)` with 256-byte random text bodies and
`probes(origin, seq, sent)`, both replayed. Version 2 adds
`tag TEXT NOT NULL DEFAULT ''` with an index and the migration
`UPDATE items SET tag = …` over every row. Rows are written 100 per
transaction: Rocky refuses a transaction with more than 256 statements or a
frame over 128 KiB.

A probe is one committed row. Its latency is the time from just before the
writer's `transaction()` to the moment the receiver's `observe_tables`
reports it, both on the driver's clock. The pair-symmetric figure averages
A→B and B→A, which cancels any remaining clock offset.

Mesh members join as writers through the creator's invitation, every pair is
introduced with `add_peer`, and every node runs with `set_automatic_sync(true)`;
without that call Rocky only syncs on explicit `sync_once`.

## Run

Build on every host (`cargo build --release` here, the same tree copied to
`~/rocky-mesh-src` on the others), edit `HOSTS` in `drive.py`, then:

```sh
python3 drive.py local     --out results/<date>
python3 drive.py loopback  --out results/<date> --rounds 40
python3 drive.py latency   --out results/<date> --modes p2p relay --rounds 20
python3 drive.py bootstrap --out results/<date> --modes p2p relay --rows 50000
python3 drive.py migration --out results/<date> --rows 50000
python3 drive.py offline   --out results/<date> --rows 20000 --tiers 1000 10000 50000
python3 analyze.py results/<date>
```

`p2p` binds each host's LAN address; peers on other networks are reached by
iroh's hole punching with the default n0 relays as rendezvous. `relay` uses
the default n0 relays only.

The root [AGENTS.md](../../AGENTS.md) and the
[benchmark instructions](../engine-comparison/AGENTS.md) apply here too.
