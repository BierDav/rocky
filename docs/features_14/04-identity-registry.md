# 4. Identity registry

Status: planned (2026-11-05). Needs phase 1; uses phase 3's membership view
when present.

## Registry

- `registry::Record` keyed by node becomes `IdentityRecord` keyed by
  identity: `profile`, `grants` (partition), `capabilities`, `origin`,
  `kind`, `issuer`, `enrolled`, `invitation`, `label`, `revoked`,
  `single_use`. `Device` and `Device.user` are removed.
- `claims` and `trusted` stay keyed by identity and are read together with
  the identity record.
- `MAX_DEVICES` becomes `MAX_IDENTITIES = 256`. `RegistryFull` names
  identities.
- The registry stores no nodes. Linked nodes come from the membership view
  (phase 3).

## Calls

| Removed | Replacement |
| --- | --- |
| `devices` | `identities`: each identity's record, its linked nodes (key, author number, linked time, self-reported name, last seen) and its trust and claim state |
| `device_label {node}` | `identity_label {identity}` |
| `revoke {node}` | `unlink {node}` with the identity key (phase 3); issuers use `revoke {identity}` |
| activity `device_enrolled`, `device_revoked` | `identity_admitted`, `identity_revoked`, `node_linked`, `node_unlinked` |

- `DeviceKeys` becomes `NodeKeys { identity, node }`. `Database::device`
  and `device_of` become `node_keys` and `node_keys_of`.
- A node's self-reported name travels in its contact (discovery
  announcement), at most `MAX_LABEL` bytes. It is not issuer-written.

## Done when

- Paperless's DatabaseSettings data needs come from `identities` alone.
- Partition grants given to an identity reach a node it links later without
  another grant.
