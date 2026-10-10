# Phase 11 chain evidence

Measured on macOS arm64 in the `p11-chains-by-reference` jj workspace, based on
`main` 4f43c453, with the pinned Rust 1.99.0 debug test profile, on a host
shared with other builds. These are local results, not WAN or phone latency
claims. [Decision 0053](../../../../docs/decisions/0053-chains-by-reference.md)
records the formats; the
[partition guide](../../../../docs/runtime/partitioned-scopes.md#four-table-profile)
sets the figures beside phase 3's.

## Fixtures

`four-table.log` is `measure_200_partition_group_sync`, `single-table.log` is
`measure_200_partitions`, each run once, unsampled:

```sh
cargo test -p rocky-secure --lib measure_200_partition_group_sync --locked -- --ignored --nocapture
cargo test -p rocky-secure --lib measure_200_partitions --locked -- --ignored --nocapture
```

Four tables took 4.357 s to register and author, 18.482 s to catch up and
1.454 s for 801 checkpoints, against budgets of 60, 100 and 48 s. The single
table took 1.450 s, 1.484 s, 0.254 s and 0.063 s to reopen, against 15, 25, 12
and 5 s. Phase 3's last run of the same fixture is in
[its evidence](../catch-up-2026-10-10/README.md); the two runs did not share a
host load, so the times compare stages, not percentages.

The byte counts do not depend on load. The 801 frames of the four-table
catch-up hold 0.59 MB of headers where they held 7.27 MB; their chains are
41,936 bytes, stored once, where 6.80 MB travelled inside the headers. The 801
checkpoint records are unchanged at 11.43 MB, 9.87 MB of it their signers'
chains, which this phase leaves in the signed checkpoint.

## Gate measurements

`chains.log` is `cargo test -p rocky-secure --lib tests::chains -- --nocapture`.

| Measurement | Before | After |
| --- | ---: | ---: |
| Frames of a 20-grant author in a 128 KiB page (1,213 B frame, 32,434 B chain) | 3 | 32 |
| Chain bytes a session carries for 40 such frames | 1,297,360 B | 32,434 B |
| First page's strong state, two 20-grant chains (37,222 B each) | 79,116 B | 3,144 B |
| First page's strong state, two 40-grant chains (54,229 B each) | 113,165 B | 3,162 B |

"Before" is computed from the same run: a frame's weight plus its chain, and
the strong state encoded with its chains embedded as the page carried it. The
page's plaintext budget is 64,512 B, so neither strong state fitted before.
The 40-grant chain has 20 grants of the fixture's full rights and 20 that only
read: 40 full grants exceed the 128 KiB a confirmed command may take, a bound
this phase does not change.
