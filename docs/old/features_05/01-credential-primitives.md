# 1. Credential primitives

Status: slices 1–5 implemented through Rust, the native ABI and Kotlin;
qualified on JVM desktop/loopback; required software result met. Owner: `rocky-credentials`, secure admission,
`sdks/native` and the Kotlin credential API. Depends on nothing; wave 0.
Follow the [execution contract](README.md#agent-execution-contract).

## Inputs and objective

Read [section 7](agreed-scope.md#7-credential-primitives) and
[section 3, the `id`](agreed-scope.md#the-id) of the scope,
[credentials](../runtime/credentials.md),
[SDK enrollment](../runtime/sdk-enrollment.md) and
[decision 0023](../decisions/0023-credentials-profiles-and-key-epochs.md).
Inspect the chain format, `Authority` issuance, the executor registration
path of the coordinator and the Kotlin profile functions.

Expose what the chain format already holds and add two new formats, so that an
application composes its own permission model without named roles, built-in
groups or an SDK sharing flow. Partition selectors and key envelopes are
**not** this phase; they ship with [phase 11](11-partitioned-scopes.md),
whose decision record covers them together with partition tuples.

## Ordered slices

1. **Raw grants.** Issue any set of scope, rights and columns, including
   `Execute` and `Custody`, to a device that is not an issuer, through the
   native ABI and Kotlin. The reader, writer and issuer profiles become
   functions over this call and stay as the optional starter layer.
2. **Delegation.** Expose chain delegation that narrows a capability. A
   delegate can never widen rights, scopes, columns or validity. Verify
   attenuation at every admission caller, not only at issuance.
3. **Executor registration.** Register an eligible executor with the
   coordinator from any device holding `Execute`, not only the issuer profile.
4. **Author pseudonyms.** A separate signing key and a 31-bit author number
   per device and table, bound in the credential; a range of author numbers
   per issuer, narrowed by delegation; author 0 reserved. Receivers verify
   that a frame's author number belongs to the key that signed it. Write the
   decision record; this is a new credential format with versioning and a
   refusal path for unknown versions.
5. Document the primitives as the supported surface and the profiles as a
   layer built only on them, in [credentials](../runtime/credentials.md).

## Completion gate

- A device without the issuer profile receives `Execute` or `Custody` through a
  raw grant and the runtime admits exactly those rights.
- Delegated chains that widen any dimension are refused on verification and
  at every protected runtime boundary.
- A frame signed by a pseudonym key with an author number outside the
  credential's range is refused; the issuer can map a pseudonym to its device
  and nobody else can.
- Existing credential files and invitations still open; a chain of the new
  version is refused by an old runtime with a clear error.
- No Kotlin path needs Rust; `Credentials.prepare` with `RockyDatabase.install`
  still lets an application bring its own issuer.

## Measured evidence

Measured on 2026-10-07 in the dedicated phase-1 workspace:

- Slices 1–3: `dd0bf0a4` (raw grants, delegation and executor registration).
- Author pseudonyms and durable allocation: `80ca4e20`; issuer assignment-key
  separation and requester-disclosing prefix refusal: `975254e4`.
- `cargo test -p rocky-credentials -p rocky-sdk --locked`: passed.
- `cargo check --workspace --all-targets --locked` and workspace Clippy with
  warnings denied: passed.
- `cargo test -p rocky-sdk --test pseudonyms --locked`: 3 passed, including
  failed persistence, lost answers, reopen, policy renewal, issuer range splits,
  delegation and issuer self-assignment without user/node identities in the
  public chain.
- `cargo test -p rocky-secure --lib tests::delegation --locked`: passed, checking
  range widening with rights, columns, scope, validity and depth at open,
  renewal, frame receipt, sync paging, acknowledgment and remote read.
- `cargo test -p rocky-sdk --test service --locked`: 2 passed, including a
  non-issuer granted `Execute` registering and executing through a service.
- `./scripts/check`: passed formatting, workspace Clippy/tests/doc tests and
  the SDK tests without default features (iroh).
- `./sdks/native/check`: passed with bindgen/default features and without iroh.
- `./sdks/kotlin/check`: passed the existing JVM SDK contract qualification.
- `./examples/shoppinglist-app/check`: 24 Kotlin/JVM tests passed; desktop build
  passed. The pseudonym tests cross the generated native bindings, including
  per-table signing keys, private issuer lookup, retry/reopen, exhausted range
  preservation through credential replacement and explicit legacy range claims.
- `./sdks/kotlin/check-packaged`: passed with only packaged jars and a JVM at
  launch, including raw grants, delegation, non-issuer execution and pseudonyms.

Requests and assigner-local records are private inputs: an assigner sees the
requesting device. The sealed-device privacy guarantee concerns published
credentials and authored rows. Issuers use an independent assignment key, and
delegates refuse prefixes naming the requesting user's identity.

The required software result is implemented. Author-number-bearing frames and
row IDs are phase 7; this phase qualifies `CredentialChain::verify_author` as
the receiver primitive, including wrong number, range, table and key refusals.
Legacy CBOR/signatures, identity migration and invitation compatibility are
covered by tests. A fixture of the old capability decoder drops the extension
and then refuses its signature, matching the old runtime's credential-error
path. Unknown extension versions get the new runtime's explicit upgrade error.
An old binary has not been launched against the new format; version/refusal
rules are recorded in decision 0030. Qualification
is bounded to JVM desktop/loopback and is not a cryptographic audit.
