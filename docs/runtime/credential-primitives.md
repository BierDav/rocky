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

- Without `columns` a grant covers every column, including those a later
  generation adds. Columns narrow writes; a
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
capability to another identity, signed with its own identity. The
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

An issuer renews only identities the owner or an issuer granted, so a
delegated chain is renewed by its delegator delegating again; the delegate
calls `install`. A
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
  tenant key and a coordination `grant` the owner signed once with
  `command_coordination_grant`. A device without `Execute`, a missing grant
  or one for another tenant is refused. Later control revisions need only
  the tenant key.

## Principal ranges and node authors

`Authority::issue` and `issue_delegable` require an explicit `AuthorRange`.
A principal grant carries that range of positive 31-bit numbers. The
principal certifies each node with exactly one number from that range.
`CredentialChain::complete(holder, node, author)` supplies the number explicitly;
`device_chain` takes the same final argument. The node key is the leaf subject
and signs every table's frames, existence records and checkpoints. Row identities
combine its author number with the table's durable creation sequence.

`verify_author` checks the node key, exact number, table grants, expiry and
revocations. Partition frames use `verify_partition_author` with the authenticated
tuple. Every node leaf attenuates its principal's range; a leaf with no number,
several numbers or an out-of-range number is refused. A replica durably binds a
node to its first verified number and refuses a different number across tables,
renewal and reopen. A principal's certificate allocator must give distinct
numbers to its nodes and retain allocations after revocation or loss. In the
SDK that principal is an [identity](credentials.md#identities): a stored
identity's one node takes its range's first number; nodes of a supplied
identity take numbers its `IdentityProvider` reserves from the bottom of the
range, at most 256 and half of it, and the node hands out only numbers above
them. Issuer authority may grant any range: ranges between issuers are an
allocation discipline, and verifiers enforce them below an issuer.

The SDK creator uses author 1 and starts allocating at 2. Ordinary enrollment
and grants reserve up to 1,024 available numbers by default; `authors` supplies an explicit count.
An issuer invitation hands out the upper half of its free range. Allocation is
saved before credentials leave the SDK. Registry capability entries record the
granted principal range, so a later grant for that principal can reuse it.
Delegators also persist outgoing ranges locally, including non-issuer grants.
Default grants keep half of a small free range for later recipients.
Neither opening a database nor creating a row contacts an author assigner.

### Chain format

A capability is signed under `rocky.capability.v2` and names no manifest: a
chain holds in every generation of its domain. Grants select fields as
`All`, every field in any generation, or `Only` named ones.
`Capability.extension` is canonical CBOR of a version-4 body. Every field is
explicit; missing fields, unknown fields, noncanonical bytes and other versions
are refused.

| Body field | Content |
| --- | --- |
| `version` | 4 |
| `authors` | `{first, last}` within `1..=2³¹−1`; one number on a node leaf |
| `node` | Node reference bound to the leaf subject; absent on principal links |
| `issues` | Issuer authority of an identity's link; see [identities](credentials.md#identities) |
| `partitions` | [Selector grants](partition-credentials.md#issuance-and-verification) |
| `issues` | Issuer authority of an identity's link; see [identities](credentials.md#identities) |

There are no table signing keys, assignment messages or sealed device bindings.
Portable `AuthorBinding` provenance is version 2 and terminates at the node key.

## Qualification

`cargo test -p rocky-credentials` covers range attenuation, exact node numbers,
selector preservation, table grants and refusal of other extension versions.
The phase's [lane B evidence](../features_09/13-identities-and-domain-credentials-evidence.md#lane-b--author-ranges-and-node-signing)
records secure storage and SDK verification separately.
