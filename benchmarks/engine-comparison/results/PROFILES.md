# Recorded profiles

All measurements are sequential local release runs. Do not combine these profiles into one average.

| Raw file | Purpose / source distinction |
|---|---|
| measurements.jsonl | 270 matrix runs plus an invalid pilot-lock record (271 records). Compact symbolic operation markers. Source: evidence/matrix-main.rs.txt. |
| crypto.jsonl | 9 initial common crypto controls with compact declared metadata. |
| focused.jsonl | 108 decoded-image/revision-observer/longer/memory/NORMAL cases. Source: evidence/focused-main.rs.txt; safe consumer remains deferred on WAL. |
| consumer-followup.jsonl | 18 cases reserve WAL write access before scanning/advancing cursor; fixes foreground upgrade starvation. SQLite size accounting still closes its last active WAL connection. |
| storage-followup.jsonl | 18 cases retain an active SQLite WAL connection and Turso database; valid matched-lifetime file sizes. Compact markers. |
| lag-followup.jsonl | 18 lag cases plus one replacement of a producer-connection initialization lock failure. Acknowledgment timestamp races have explicit missing counts. Source: evidence/lag-main.rs.txt. |
| logical-followup.jsonl | 18 matched-lifetime cases with complete declared JSON operations; confirms explicit MVCC advantage with credible larger payloads. CDC captures framework metadata payload as well as projection rows. |
| crypto-logical.jsonl | 9 final equivalent serialization/signing/verification/encryption/decryption controls with complete operation bodies, persisted signature + nonce + ciphertext. |
| correctness.jsonl / crash.jsonl | Final-source results, six adapters each. Empty correctness.stderr means the assertions passed. |
| pilot.jsonl / pilot2.jsonl / storage-pilot.jsonl | Superseded debugging runs; excluded from all summaries. Retained for transparency. |

Two invalid non-pilot measurement records remain visible: a file held by an orphan pilot process, and capture-connection initialization racing a consumer. Both were replaced with successful runs. The final source prepares producer capture connections before starting consumers, uses bounded whole-mutation retries, validates complete captured transaction coverage, and asserts SQLite WAL lifetime during size sampling. summary.csv/json include only successful records; failed cases appear in MEASUREMENTS.md.

Per-run commands are stored with every raw record. matrix-sha256.txt and final-sha256.txt identify the corresponding Rust source and Cargo.lock. Plotting dependencies are separately pinned in plotting-requirements.txt. Reproducible database files are removed after sizes are recorded; raw outputs and errors are retained.
