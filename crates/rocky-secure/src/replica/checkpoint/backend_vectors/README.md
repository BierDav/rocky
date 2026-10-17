# SHA-2 byte fixtures

These are fixed answers captured before the round48 backend overlay on
2026-10-21. The pre-change SHA2 package was 0.10.9 with only default/std;
its AArch64 route was software. The root lock SHA-256 was
`da7b94cd0c84a4556fc588116cceae3283c6059b915ce2c24ca6f0c1d552459a`.
The final software fixture source snapshot was
`d11c2afe7875eac951e1e87dcada1c2e4d9a3259`.

`verify.py` independently reconstructs every full 2,048-byte lane state and
4,120-byte Sets state using Python's standard `hashlib`, including repeated
addition across u16 wrapping. It checks the frozen statement's roots, key-epoch
commitment, signing envelope and checkpoint ID. It reads the fixtures without
replacing them:

```sh
python3 crates/rocky-secure/src/replica/checkpoint/backend_vectors/verify.py
```

The Materialized CBOR records and complete checkpoint encodings came from the
actual pre-change production encoders and signing API. The test reconstructs
those same values, compares every byte and verifies the old signed fixture.
The public test seeds are owner `[40;32]`, identity `[41;32]` and node `[42;32]`;
credential issuance is fixed at 1,800,000,000 for 86,400 seconds and verification
at 1,800,000,001. Scope, tip, frontier, covered marks and nonempty roots are fixed
in `backend_vectors.rs`. No wall clock or random ciphertext is involved.
Signature and root tampering must fail the real checkpoint signature gate.

The core table in `rocky-core/tests/hash_vectors.rs` was also generated before
the overlay using Python hashlib over explicit little-endian u32 UTF-8 prefix
lengths, prefix bytes and deterministic binary payloads. Its raw SHA256/SHA512
answers include both padding boundaries. The existing genesis answer stays fixed.

The software core run passed four tests; the three secure fixture assertions
passed individually in 10–13 ms under the existing 2 s integration budget.
The temporary capture test was removed before enabling the backend. Test source,
capture source, fixture hashes and full execution provenance are retained in
`/tmp/rocky-round48-*`; the implementation handoff identifies their final paths.
These fixtures establish byte identity, separately from hardware selection and
end-to-end performance or platform qualification.
