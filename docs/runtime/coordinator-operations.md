# Operating a coordination service

This runbook makes a deployment reviewable; it authorizes none. Deploying or
advertising a public endpoint needs a separate request and concrete provider
configuration. Concepts are in [public coordination](public-coordination.md),
storage and recovery boundaries in [shared coordination](shared-coordination.md).
A provider service and a self-hosted one follow the same steps; only who holds
the root key and who may register differs.

## Keys and where they live

| Key | Held by | Used for |
| --- | --- | --- |
| Root | Offline, by the operator | Certifying genesis once; never on a voter |
| Membership authority | Operator workstation | Initializing, membership changes, status, snapshots |
| Tenant admission | Provider's account system | Signing `TenantAdmission`; omit for open registration |
| Voter key and client secret | Each voter host only | Voter signatures; the voter's iroh endpoint identity |

Key files are owner-only plaintext. `rocky-voter` never prints a private key and
never replaces an existing key file. `rocky-voter key FILE` writes a private key
and prints only its public key for the plan. Protect configuration files
containing private keys with owner-only permissions too.

## Provision

1. On every voter host: `rocky-voter keys DIR PUBLIC_UDP_ADDRESS`. It writes
   `voter.key` and `client.secret` and prints the public key and iroh endpoint
   address to certify.
2. Write `plan.json`: hexadecimal `mesh` (16 bytes) and `configuration`,
   `membership_authority`, optional `admission`, the voters (`id`, provider-internal
   TCP `address`, `key`, `client`), `max_control_lifetime`, `tenants`,
   `registrations_per_day`, `tenant_lanes` and optional `limits`. The genesis is
   refused unless `tenants` times the tenant byte and retry budgets fit the
   service's.
3. Offline: `rocky-voter certify plan.json root.key OUT`. It writes
   `genesis.cbor` and `bundle.json` and prints the genesis ID. Put the root key
   away again.
4. On each voter, a configuration `{data, genesis, trusted_root, mesh, node, key,
   client: {secret, relay, discovery, bind}}` and the first start with
   `rocky-voter CONFIG.json --provision`. Later starts omit `--provision`.
5. `rocky-voter admin ADMIN.json initialize`, where `ADMIN.json` holds
   `{genesis, trusted_root, mesh, key, evidence}` of the membership authority.
6. Publish `bundle.json` only after `rocky-voter admin ADMIN.json status 1`
   shows a leader and the service has been tested.

Voter TCP stays inside the provider network: on a public service it answers
only the membership authority. Tenants use the iroh client lane, whose endpoint
IDs the genesis certifies. Relay and address discovery are the provider's
choice; a client endpoint address must remain reachable at the certified ID.

For development, `scripts/coordinator-local DIR [BASE_PORT]` runs these steps on
loopback with independent secrets per voter, keeping the root key in memory
only, and starts three voter processes. Nothing it writes is a production key.

## Container image and deployment shape

The image jobs of the [baseline workflow](../development.md#ci) build `rocky-voter` from
`crates/rocky-coordinator/Dockerfile` for `linux/amd64` and `linux/arm64` on
native runners, merges them with `docker buildx imagetools create` and pushes
`ghcr.io/bierdav/rocky-db-coordinator:main` and `:sha-<commit>` on qualifying
`main` pushes; pull requests only build. The image is the binary as entrypoint
and carries no keys, configuration or trust bundle. The GHCR package stays
private; give the cluster a `ghcr-pull` registry secret. Pin the manifest digest (`:main` moves; Argo CD does not roll a
workload when a tag moves), kept current with Renovate.

Run one voter per node as a DaemonSet with host networking and a node-local
store: the genesis fixes each voter's consensus address and iroh endpoint to its
node. Mount the per-node configuration from a Secret keyed by node name. The
cluster manifests and guide live in the `selfhosted-platform` repository
(`docs/coordinator.md`); genesis and keys are never committed. The coordinator
holds metadata only; application ciphertext custody is the separate
[always-on peer](always-on-peer.md).

The `busy` status counter includes connections refused when all 64 voter TCP
slots are occupied, as well as full client and tenant lanes.

## Observe

`rocky-voter admin ADMIN.json status VOTER` reports leader, applied index,
voters, fence, retained logs and snapshots, disk bytes, sessions, retained
responses, accounted state bytes, retry records, tenants, and counters of
requests, linearizable reads, lane refusals, denials and unavailable answers.
It contains no keys, payloads or request contents. Voter logs record lifecycle
events only. Rotate them at 10 MiB and retain at most two files per voter;
retain exported metric samples for at most seven days. Alert on: no leader,
`fenced`, state bytes near the budget, growing `busy` or `unavailable` counters,
disk growth beyond two snapshots and the log cap, and voter clocks differing by
more than a few seconds.

## Restart, membership and keys

Restart a voter by stopping and starting it without `--provision`. Its store
lock refuses a second process. Restart one voter at a time and wait for a leader
between them.

Change membership with the next revision and both sets, for example
`rocky-voter admin ADMIN.json membership 0 1,2,3 1,2`. Retrying the same plan
is safe. Only certified candidates can join, and at least two voters remain.
Replacing a voter key, a client endpoint ID or the root needs a new genesis:
a new configuration with client trust migration.

## Back up and restore

`rocky-voter backup CONFIG.json OUT` copies a stopped voter's store with a sealed
`BACKUP` manifest; a running voter is refused. Backups hold the same bounded
state as the store, no lifetime archive and no application payload. Keep at
most two completed backups per voter, including exported copies; validate a
new copy before deleting the oldest.

Restore one lost voter:

1. Remove it: `membership REV 1,2,3 1,2`. This discards the leader's remembered
   replication progress for it.
2. `rocky-voter restore BACKUP CONFIG.json` into an empty data directory of the
   same voter, then start it. Restore uncommits the stored vote and writes a
   sealed `RESTORED` fence: no vote, no election, no clients.
3. Re-add it: `membership REV+1 1,2 1,2,3`. The fence lifts only after the voter
   observes a learner membership beyond the backup's last log index followed by
   an applied voter membership; `status` shows `fenced: false`.

Restoring before removal violates OpenRaft's replication contract and can
interrupt the leader. Restoring every voter leaves the whole service fenced and
unavailable: a backup never silently rolls a service back, and clients' head
watermarks would refuse it anyway.

## Capacity

Accounted state is bounded by the genesis: at most `tenants * tenant_bytes`
within the service budget of at most 24 MiB, plus bounded logs (snapshot every
1,024 entries, two snapshots kept). Each voter answers at most 64 concurrent
client requests, `tenant_lanes` per tenant. When tenants report `Backpressure`
for state, admit fewer tenants per group or run another group; raising budgets
needs a new genesis and measured evidence.

## Outages and permanent loss

With a quorum missing, strong work waits and nothing is confirmed; restore the
quorum and clients complete the same identities. Correct clocks without resetting
committed authority time. Losing a quorum's stores permanently cannot be
repaired from a minority or from backups: deploy a new configuration, migrate
client trust and recover application results from the devices' retained
evidence and payloads, as [shared coordination](shared-coordination.md#durability-and-recovery-authority)
describes.
