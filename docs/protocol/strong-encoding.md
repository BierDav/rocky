# Strong request/package encoding vectors

The [strong-command contract](strong-command-contract.md) specifies fixed field
order and domain separation. `rocky-core` implements generic request/package/
confirmation/receipt encoding and SHA-256 commitments. Todo owns state row bytes.

All integer fields are little endian. IDs/configurations are 16 raw bytes;
commitments are 32. Sequence/epoch fields use u64; counts, lengths, generations and
method versions use u32. Command IDs encode node then command sequence. Frontier
and required operation IDs are sorted unique. Rows sort by entity operation ID.
Bool is a single byte, 0 or 1; title is length-prefixed exact UTF-8.

An embedded request, state-row object or accepted confirmation is length-prefixed.
In particular, package rows encode `u32 byte_length` followed by the state bytes
(`u32 row_count`, then entity/title/bool tuples). This makes the generic package
codec independent of application row shape. The package's repeated frontier and
generations must equal its request. There are no JSON/debug serialization hashes.

Each hash starts with `u32 prefix_length` and ASCII prefix bytes, then its payload.
Request and package wire bytes do not themselves include these hash prefixes.
Successor head payload is previous head, successor sequence, request hash, result
commitment. Genesis payload is mesh/app/domain/configuration.

Phase 1 adds a caller principal (16 bytes) at the start of confirmation bytes;
receipts length-prefix that new confirmation representation. Old receipt bytes are
incompatible. Request/package encodings and all commitments below remain unchanged.
`StrongHead.scope` must equal the embedded request scope and is not duplicated in
the package encoding. See [compatibility](../runtime/local-replica.md#audit-and-offline-verification).

## General command extension

Phase 2 appends an optional canonical request extension after the sealed digest:
`u32 2`, length-prefixed argument bytes, 32-byte policy reference and 32-byte
caller-supplied request handle. Empty arguments with zero policy/handle omit the
extension; explicitly encoding that all-default extension is rejected. All bytes
participate in request/package/head commitments. The fixture below retains its
original default fields and golden hashes. Installed runtime submissions bind
their actual caller handle and therefore carry the extension.

Application results may encode strong state, rejected decisions and job intents;
they are not restricted to snapshot rows. Reconstruct results from the sealed cut
and exact confirmed predecessor before application. See [app runtime](../runtime/app-runtime.md).

## Golden fixture

Use fixed fixture mesh `11`, app `22`, domain `33`, owner `44`, Laptop `55`, Phone
`66`, each repeated for 16 bytes. Configuration is `07` repeated 16 times. The
request is Phone command 1, `PublishListSnapshot`, version/schema/handler 1, empty
frontier and empty visible rows. Its package is Laptop epoch 1, expected sequence
0/genesis head, publication sequence 1, empty required IDs and empty state bytes
`00000000`. Request wire size is 159 bytes; package wire size is 275 bytes.

| Commitment | SHA-256 hex |
| --- | --- |
| Genesis | `48ead3a3d0babf04aee931f6f43e86c2f38453111304988dd63867dcdf87469a` |
| Empty state | `6300e5b2723812489cc0ab00decd4030a15bee4543acde071b091ee2b9c9ee70` |
| Request | `064b3440f48ab38f1c0e6fbb7423f496cb0e2ea332ed4fda5698e407f5ac11b8` |
| Package/result | `29c1567673ac5baf7f7a80a62b6b08dddba29ad44cc87aeed63028756537c4ea` |
| Successor head | `af37782eb3a0cb627934a1678f13649762a60b80983694227cbce08212176a6b` |

These constants were independently derived using Python `struct` and `hashlib`.
`crates/rocky-core/tests/strong.rs` checks them, round-trips all generic objects,
and rejects every truncated package prefix plus trailing bytes. Decoding checks
embedded lengths/counts against remaining bytes before allocation and bounds
embedded byte objects to 16 MiB. Todo additionally bounds title size and validates
UTF-8, row ordering and booleans.

A nonempty state fixture has one row, Laptop entity sequence 1, title `☃`,
completed true. Its bytes are
`0100000055555555555555555555555555555555010000000000000003000000e2988301`;
its state digest is
`2eeac0e10b0c436bd398a96c5ccead167656c80fc3ce2b8be8c48623ca071962`.
`todo-app/tests/publication.rs` checks exact bytes/UTF-8, digest and malformed rows.
