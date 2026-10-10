# Partition credential primitives

The `rocky-core` and `rocky-credentials` primitives behind
[partitioned scopes](partitioned-scopes.md): addresses, selector grants and
group secret envelopes. That guide covers the Kotlin/native lifecycle and formats.

## Addresses and selectors

`rocky-core::PartitionValue::new(KeyColumn, SqlValue)` applies the existing
business-key affinity and collation codec. A null or oversized value is refused.
`PartitionTuple::new` takes stable column commitments and these values. It
refuses duplicate columns, empty tuples, more than 16 columns and an encoded
tuple above 1 KiB. `tuple.scope(domain, table)` derives the encryption scope
as the commitment over domain, table and tuple under
`rocky.partition-scope.v1`; the table and ordering domain remain the table's
existing commitments.
SQL-equal keys derive the same scope, including NOCASE/RTRIM equivalence.
Different domains, tables, columns or values derive different scopes.

A `PartitionSelector` maps **every** partition column to `Any` or
`Only(nonempty set)`. For an items table partitioned by customer and warehouse,
selecting customer X and `Any` warehouse includes every warehouse of X. A
selector missing a partition column neither matches the tuple nor narrows a
complete selector. Finite selections have at most 256 values each and the
aggregate value-byte budget is 16 KiB. Canonical component validation runs
before matching or attenuation, including for deserialized inputs.

Tuples and selectors serialize as sorted arrays of column/value pairs, also in
JSON; there are no byte-array map keys. Values are canonical key-codec bytes,
so a 64-bit integer is never converted to a JSON floating-point number.
Deserialization refuses duplicate column commitments. This is the portable JSON contract consumed by the SDK and Kotlin wrappers.

## Issuance and verification

`rocky-credentials::PartitionGrant` pairs a selector with its domain, table,
row selection, rights and field set. `Authority::issue_partition_terms` issues
selector grants beside optional concrete `ResourceGrant`s and an author range.
It shares the existing revocation, validity and delegation-depth contract.
`CredentialChain::delegate_partitions` narrows selectors, rows, rights, fields,
validity, depth and author ranges. One parent's grant must cover each child
grant: combining the rights of one parent grant with the fields of another is
refused, both at issuance and when verifying a hand-signed chain.

`complete` preserves selectors in the node leaf while dropping its assignable
author range. `Authority::renew` preserves the root's selectors under the new
policy, and refuses a revoked subject. `provenance` verifies historical identity
without authorizing any partition.

`verify_partition` verifies the complete node chain under the supplied publisher,
manifest, current signed policy statement and time, then resolves its grants on
one concrete tuple and scope. It refuses a tuple whose derived encryption scope
does not match, a wrong subject or a partition outside the leaf's selectors.
Custody-only grants pass these checks without any reader key. A caller still
checks that the returned concrete grants permit the requested action.

`assign_partitions` produces a per-table author pseudonym with partition grants
and a device binding sealed to the publisher. `verify_partition_author` checks
its signing key, author number, table and concrete partition, then returns its
paired grants. An insert/update/delete is admitted only if the precise row
write passes `RowWrite::permitted_by` against those grants. Issuers can still
open the pseudonym's device through `pseudonym_device`.

Every capability carries [extension version 2](credential-primitives.md#chain-format),
including an empty selector list. The ordinary `verify`, `verify_pseudonym` and `verify_author` APIs refuse a
partition leaf; callers must not drop its selectors and treat it as a table
capability. Concrete grants and selector grants attenuate independently;
converting a selector into a concrete grant inside a delegated chain is not
supported by these APIs.

## Group secrets

`rocky-credentials::derive_table_key(secret, domain, group, table, epoch)` is
HKDF-SHA256 binding the domain, group address, table identity and positive
epoch; a derived key reveals neither the secret nor a sibling table's key.

`GroupSecretEnvelope::seal` (version 1) wraps one group partition's 32-byte
secret for one epoch to one recipient's Ed25519 public key, converted to
X25519, with ChaCha20-Poly1305. The issuer signs the whole envelope under
`rocky.group-secret-envelope.v1`. `seal_many` wraps the same secret
independently to 1–256 distinct recipients, each with fresh ephemeral material
and a fresh AEAD nonce. The sealed length follows from the secret length.

`verify` pins the expected delivering issuer, manifest, group address and
epoch and checks its signature, without any reader secret. `open` additionally
requires the intended recipient's secret key. The wrapping key and associated
data bind the version, manifest, group address, epoch, recipient, issuer and
ephemeral public key. Wrong recipients, keys, contexts, zero DH shared secrets,
other versions or sealed lengths and modified ciphertext are refused.
`from_bytes` enforces a 4 KiB canonical CBOR bound; decoding alone does not
verify the issuer signature. [Decision 0040](../decisions/0040-group-secrets.md)
pins both formats.

An envelope grants no rights. The receiving application verifies credentials
and decides whether the delivering issuer is trusted for that partition. The
SDK lifecycle, holding rule and revocation are in
[partitioned scopes](partitioned-scopes.md#group-secrets).

## Qualification

`cargo test -p rocky-core` and `cargo test -p rocky-credentials` cover canonical
SQL equality, tuple separation, selector attenuation, paired rights/fields,
custody, policy/expiry/revocation, renewal, author admission, the HKDF vectors,
derived-key separation and recipient/context/ciphertext substitution.

## Design rationale

- Selectors avoid one capability grant per partition; keys, logs and
  checkpoints stay per partition.
- A child grant must fit one parent grant, so rights of one grant never
  combine with fields or partitions of another.
- A grant delivers no key and an envelope grants nothing: admission verifies
  credentials regardless of who delivered a key.
- Recipients get separate envelopes of one secret and never share a wrapping
  secret.
- One secret per group partition costs one envelope and one rotation however
  many tables the group has.
