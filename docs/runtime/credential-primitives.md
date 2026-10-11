# Credential primitives

Available through Rust, the native ABI and Kotlin. These are the supported surface an application composes its own permission
model from. Rocky defines no roles, groups, membership or sharing flow; the
reader, writer and issuer [profiles](credentials.md#grant-profiles) are an
optional starter layer built only on these calls. Formats, chain rules and
key epochs are in [credentials](credentials.md).

## Raw grants

A raw grant names one collection, a set of rights and optionally the columns
those rights cover. Any set is issuable to any device, `Execute` and `Custody`
included; the device does not become an issuer.

```json
[{"scope": "items", "rights": ["read", "update", "custody"], "columns": ["completed"]},
 {"scope": "notes", "rights": ["custody"]}]
```

| Right | Effect |
| --- | --- |
| `read` | Receives the collection's reader keys of every epoch; reads whole rows |
| `insert`, `update`, `delete` | Writes rows; `update` and `insert` only the named columns |
| `custody` | Holds and serves the collection's ciphertext without reading it |
| `invoke` | Submits the commands whose arguments are sealed for the collection |
| `execute` | Executes the commands that read or write the collection |

- Without `columns` a grant covers every column. Columns narrow writes; a
  reader key still reveals whole rows, as [reader scopes](credentials.md#limits) do.
- `invoke` and `execute` are refused on a collection no command uses, and
  `publish_schema` is no collection right. Unknown collections, columns and
  rights, an empty right set and an empty grant list are refused; a
  credential holds at most 64 grants.
- A set of grants that is exactly a reader or writer profile is reported as
  that profile; any other is reported as `{"kind": "grants", "grants": [...]}`.

`Grant` and `Grant::resolve_all` in `rocky-credentials` turn raw grants into
signed `ResourceGrant`s; `Profile::raw` is how each starter profile produces
them, and `Profile::Grants` carries raw grants wherever a profile is accepted.

### Issuing raw grants

| Call | Inputs | Result |
| --- | --- | --- |
| `grant` (issuer) | `device`: the keys `Database::device` / `Credentials.prepare` reported; `grants` or `profile`; optional `ttl` (60 s to a year), `delegations` (1 to 7, default 1), `authors` (count) | `supplied`: the bundle `Database::install` takes |
| `pairing` | `profile`, including `{"kind": "grants", ...}` | An [invitation](sdk-enrollment.md#invitations-and-pairing) for exactly those grants |

```kotlin
val keys = Credentials.prepare(directory)                       // on the new device
val supplied = issuer.credentials.grant(keys, listOf(Grant("items", setOf(Right.Read, Right.Custody))))
val device = RockyDatabase.install(directory, schema, supplied)  // no Rust involved
```

The issuer lists the device among its `devices`, serves it and can revoke it
like an enrolled one. `supplied` carries reader keys: hand it over like an
invitation. Any issuer renews a granted device with the same grants.

## Delegation

A device whose capability allows a further link delegates a narrowed
capability to another device's user key, signed with its own user key. The
issuer allows this with `delegations` above 1 when granting: 1 reaches only
the device's own node key, 2 one delegate and its node, and so on, up to the
chain depth of 8.

| Call | Inputs | Result |
| --- | --- | --- |
| `delegate` | `device`, `grants` or `profile`, optional `ttl`, `delegations`, `authors` (a count of free author numbers) | `supplied` for the delegate |
| `install` | `supplied` | Replaces this device's credentials; the new `session` |

A delegate never holds more than its delegator:

- Every grant must be included in one of the delegator's: the same scope, a
  subset of its rights, columns and rows.
- Its validity is clamped inside the delegator's and its depth is lower.
- The delegator's reader keys travel with it only for collections the
  delegated grants read.

`CredentialChain::delegate` refuses a widening link when it is made, and every
verifier refuses one that a holder signed anyway: opening a replica, renewing
credentials, receiving a frame, serving a sync page, an acknowledgment, a
remote read, a command or a recovery transfer all run the same chain check.

An issuer renews only chains it issued directly, so a delegated chain is
renewed by its delegator delegating again; the delegate calls `install`. A
[revocation](revocations.md) naming the delegator's key or capability ends
the chains delegated from it. The delegate's sync peers are configured with contacts, as for any member.

## Executor registration

Execution needs a coordinator assignment, `Execute` on every resource of the
command and the complete confirmed state; nothing else about the device
matters. A device granted `Execute` therefore executes once registered:

- With an in-process `DevelopmentCoordinator`, register its
  `command_identity` as an executor.
- With a [coordination service](public-coordination.md#register-a-domain),
  any device holding `Execute` calls `command_service_register` with the
  tenant key and a coordination `grant` an issuer signed once with
  `command_coordination_grant`. A device without `Execute`, a missing grant
  or one for another tenant is refused. Later control revisions need only
  the tenant key.

## Author pseudonyms

A pseudonym is a separate signing key and a positive 31-bit author number for
one device and table. The device keeps its secret in the key provider; its
chain binds the public key, number and table grants. Only a domain issuer can
open the sealed device key in the chain. Author 0 is reserved.

| Call | Inputs | Result |
| --- | --- | --- |
| `pseudonym_request` | `collection` | `request`: hexadecimal bytes signed by the node and its pseudonym key |
| `pseudonym_assign` | `request` | `pseudonym`: a credential chain with the device's table write grants, narrowed by the assigner's grants |
| `pseudonym_install` | `pseudonym` | `collection`, `author`; saves the verified chain for the requesting device |
| `pseudonym_owner` (issuer) | `pseudonym` | `node`, `collection`, `author` |
| `pseudonym_range` (issuer without a range) | `first`, `last` | Claims a range and renews the issuer's capability with it |

The application carries private requests to a trusted assigner and returns
the credential; no sharing or invitation flow is required. Requests and local
assignment records disclose the device to the assigner. Published credentials
seal the device binding to the publisher. Issuers sign through an independent
assignment key, including for their own device. A delegated assigner refuses
an assignment whose parent chain would name the requesting device's user;
that device asks an independent assigner instead. Assignment requires a range and a delegable capability with the
requested grants. A reader gets no writing pseudonym. A lost assignment
answer retries with the same key and keeps its number, including after the
assigner reopens. Assignment records and counters commit to `identity.json`
before an answer leaves the SDK. A failed save returns no assignment.

```kotlin
val request = phone.credentials.pseudonymRequest("items")
val credential = issuer.credentials.assignPseudonym(request)
val author = phone.credentials.installPseudonym(credential) // collection and author number
val device = issuer.credentials.pseudonymOwner(credential).node
```

`Credentials.claimAuthorRange(first, last)` is the Kotlin range claim; it
assigns a range to an issuer that has none. `Credentials.grant` and `Credentials.delegate` take optional `authors`;
the five pseudonym calls cross the native session's versioned JSON ABI.

The creator starts with `[1, 2³¹−1]`. An issuer invitation splits the free
numbers, keeping the lower half and handing the upper half to the new issuer.
The optional `authors` count on `grant` and `delegate` carves numbers for a
recipient that assigns them itself. The receiver verifies range attenuation
at every chain admission boundary. Replacing supplied credentials never
rewinds the allocation counter, even across removal of a range. Delegating
again with `authors` carves a fresh range; omit it when the recipient no
longer needs to assign authors.

A [revocation](revocations.md) naming the pseudonym key, or a key or
capability its chain passes through, ends the pseudonym chain like a device
chain. After renewing the device credentials, request, assign and install the
pseudonym again; the key and author number stay the same. Rotation of pseudonym keys is out
of scope.

`CredentialChain::verify_author` is the receiver primitive: it verifies the
signing key, exact author number, table, grants and the held revocations. A pseudonym
chain is refused wherever a node chain is required, including existing frames
and sessions.

### Chain format

`Capability.extension` is required canonical CBOR of a format-2 body. All
fields are explicit, including an empty partition list and null author/device
bindings. Missing extensions, unknown fields, noncanonical encodings and every
other version are refused; no earlier signatures or encodings are preserved.

| Body field | Content |
| --- | --- |
| `version` | 2 |
| `authors` | `{first, last}`: the numbers this link assigns, or the single number of a pseudonym leaf. `1 ≤ first ≤ last ≤ 2³¹−1` |
| `device` | Pseudonym leaf only: the node key sealed to the publisher |
| `partitions` | [Selector grants](partition-credentials.md#issuance-and-verification) |

- A link's range lies inside its parent's; the publisher's root may carry any
  range. Node links carry no `authors`.
- A `Pseudonym` leaf has the pseudonym key as subject, `authors` `[a, a]`,
  `delegations` 0 and grants on one table. Its parent is a user or deployment
  link whose range contains `a`.
- `device` uses an ephemeral X25519 key against the publisher key in
  Montgomery form and ChaCha20-Poly1305 under a key bound to the pseudonym key
  and author number.
- `PseudonymRequest` (version 1, `rocky.credentials.pseudonym-request.v1`)
  carries the node chain, pseudonym key, table, time and an optional earlier
  pseudonym chain, signed by the node key and the pseudonym key. An earlier
  chain for the same key and table inside the assigner's range keeps its number.

## Qualification

```sh
cargo test -p rocky-credentials
cargo test -p rocky-secure --lib tests::delegation
cargo test -p rocky-sdk --test grants --test service --test pseudonyms
./examples/shoppinglist-app/check
sdks/kotlin/gradlew -p sdks/kotlin :qualification:packaged-jvm:qualifyPackage
```

Covered: raw grants resolving and describing back; chains widening rights,
columns, scope, validity or depth refused at open, renewal, frame receipt,
sync paging and remote read; a non-issuer executor; pseudonym persistence
failure, lost answers, reopen, policy renewal, issuer splits and delegated
ranges; sealed-device privacy. JVM desktop and loopback only.

## Design rationale

Status: compatibility clause superseded on 2026-10-08 by the phase-1
single-format contract.

- Every capability carries the same version-2 authority body. Ranges, the
  sealed device and partition selectors are explicit; old bytes and signatures
  are not retained.
- The device is sealed asymmetrically to the publisher key so delegates with a
  range can assign numbers without holding a publisher secret.
- A device's own user key never signs its pseudonym, and a delegated assigner
  refuses a device whose user key is in its prefix: either would name the
  user in every `id`.
- Ranges between issuers are an allocation discipline. An issuer holds the
  publisher key and could sign any range; verifiers enforce ranges only
  against delegates. Counters are never rewound.
