# 4. Compact row representation

Status: complete and qualified (2026-10-23). See [implementation evidence](04-compact-row-representation-evidence.md). Owner: `rocky-core` row and stamp types,
`rocky-secure` frames, stored records, creation proofs and checkpoint pages,
`rocky-sdk` and `sdks/kotlin` typed rows. Depends on
[phase 1](01-held-positions-as-tables.md). Effort: three to five weeks, run
as its own phase with evidence. Follow the
[execution contract](README.md#agent-execution-contract).

## Inputs and objective

Inspect the owning modules and their tests for current behavior, and the
[performance review](00-sync-performance-review.md) for the measured baseline.

After this phase a seven-field row of 50 bytes costs at most 500 bytes in
its frame and 300 bytes at rest. Field names stay the identities of fields;
authority, admission evidence and merge order decide exactly as before.

## Settled

- **Fields by ordinal on the wire.** A frame names a field by its ordinal
  in the source tip's canonical logical schema: fields sorted by name
  within their table, numbered from zero, per schema definition.
  Admission maps ordinal to name through the source schema the catalog
  already requires ("source tip schema is not installed" stays the
  refusal). Grants, the path filter, `validate_changes` and kept views keep
  working on names.
- **Fields by interned id at rest.** `secure_field_names(id INTEGER
  PRIMARY KEY, name TEXT UNIQUE)` allocates a small id the first time a
  name is seen in this file and never reuses it. Stored records, the
  overlay, scratch and shadow tables key by that id. A removed and re-added
  name keeps its id, which is what names-as-identities means.
- **Stamps carry the author number.** The secure row stamp becomes
  `(lamport, author: u32, sequence)`; register merge orders by that tuple,
  which is total within a domain because author numbers are unique there.
  Identity marks keep the signer key, taken from the header. The created
  stamp follows the same shape.
- **An ordinary birth carries no second signature.** Its creation statement
  is derived: the source tip's schema manifest, the scope's table, the
  identity, the frame's stamp (timestamp, author number, `author_sequence`)
  and the chain digest as bindings. Provenance is the commitment over the
  same canonical bytes the signed statement had, so `secure_admission_claims`,
  receipts and restoration compare unchanged. The frame signature is what
  binds it. A carried row keeps `CreationProof` with the original author's
  signature, because the carrier proves another author's creation.
- **Checkpoint pages carry a legend.** A page lists the field names it uses
  once; rows index into it. A row record carries its provenance commitment
  and derived statement fields; the page signature covers them. The
  importer maps legend names to local ids. Imported rows were already
  trusted on the signer's authority, so no verification is lost that a
  claim could use (admission evidence).
- **Before and after images are unchanged**: a write still publishes only
  the values that differ, and a delete its before image.
- **Formats.** Row frame 11 → 12, encrypted checkpoint 16 → 17, replica
  file one version up, link one version up for remote-read packages.
  Existence records, seal and bookmark statements, receipts and the
  credential formats do not change. No reader of the previous versions.
- **Kotlin and the native ABI** exchange rows by SQL column and name
  through `TypedRows`; this phase expects no ABI change and verifies it in
  slice 5 before claiming it.

## Ordered slices

1. `rocky-core`: ordinal field references, the secure stamp shape, the
   canonical creation statement with a derived constructor; vectors for
   provenance equality between signed and derived statements.
2. `rocky-secure` frames: encode and decode by ordinal, stamps by author,
   births without proofs, carries with proofs; admission mapping; row
   frame 12.
3. Stored records and the interning table; kept views, overlay, scratch,
   shadow, rebuild and audit through it; the replica version.
4. Checkpoint pages with legends; format 17; import, offer and serving.
5. `rocky-sdk` and Kotlin: typed rows, remote reads, the link version; the
   ABI check.
6. Evidence on the 100k fixture: stage times and `dbstat`.

## Completion gate

- The 100k fixture: average frame body ≤ 16 KB for 32 rows, `secure_rows`
  ≤ 30 MB, `secure_operations` ≤ 100 MB, member file ≤ 200 MB, member
  stage ≤ 60 s with phase 2 in place.
- Two devices on different app versions, one with a removed and re-added
  field, converge to the same rows; the re-added field never receives a
  value from before its removal.
- A carried row's provenance equals the original signed statement's; a
  carried row with a wrong proof is refused; a birth whose derived
  statement differs from its frame is unrepresentable by construction and
  a tampered chain digest fails the frame.
- The admission evidence suite passes unchanged: receipts, restoration,
  consumed births, two filtered births of one identity.
- A checkpoint page with a legend entry missing, repeated or out of order
  is refused and stores nothing.
