# 0056. Contact exchange and discovery secrets

Status: accepted; implemented by phase 14.
Date: 2026-10-10.

## Context

Roster-free admission accepts peers without a contact. Contacts remain local
route hints, and phase 10 supplies proof-bound frontier replies for partner
selection. Discovery must change on revocation without giving a revoked device
the new topic. Native frames authenticate but do not otherwise hide metadata.

## Decision

Native session **11** adds one contact family with three exchanges:

- Bidirectional handover, at most 16 contacts per side per session, limited to
  recently reached peers. Routes retain their four-route and 2,048-byte limits.
  SDK responses also fit a 96 KiB plaintext handover budget.
- Request by the 16-byte `secure_node_id`, at most eight per session. Unknown
  and revoked nodes both return no contact. The client checks the returned key
  hashes to the requested ID.
- Discovery root and values, paged in statement-ID order, 32 values per page.
  A value names its revocation statement; a root has no statement ID. Both
  carry publisher signatures over the domain scope and value, with the
  `rocky.discovery-secret.v1` signing label. Substitution is refused.

The signed hellos include fresh X25519 exchange public keys. The shared secret
and signed handshake derive a key under `rocky.discovery-session-key.v1`.
Contact-family payloads are ChaCha20-Poly1305 sealed under it with the
`rocky.discovery-session.v1` associated-data label. Signatures, frame sequence,
byte quotas and timeouts continue to apply. Both admission and the merged
revocation check precede this family; servers check admission again before
sending an answer. No discovery material travels in enrollment or renewal.

The creator mints the domain root. A revocation's durable rotation intent
names the statement before its discovery value is minted; retries keep the
same value. The root, values and current/previous topic state live in a
version **1** structured `SecretStore` record, never `identity.json` or the
replica. Values for expired statements are dropped with the set's cutoff.

The topic is the commitment under `rocky.discovery-topic.v1` of the root,
followed by each live statement ID and its value in ID order. Presence is
encoded explicitly; a missing value therefore derives a different topic.
The previous topic expires 24 hours after a change, including across reopen.
Phase 15 consumes `discovery_topics`; phase 14 performs no network discovery.

Identity **13** adds local contact provenance and learning time. Application
contacts cannot be overwritten or evicted by learned hints. Other contacts
age after 30 days; a full list evicts its oldest eligible learned contact.
An enrollment-forwarding upstream is retained. No roster or holdings table is
introduced. Enrollment, invitation and contact URI formats remain unchanged.

A pass ranks ready local contacts, peers whose last successful frontier
exchange held needed scopes, then trusted contacts, then probes. Backed-off
peers rank last and cost no dial. Within a rank, least-recently attempted
contacts go first. Up to 16 contacts are selected. A leading local peer is
tried alone; remaining peers run in groups of up to four. Between groups the
pass stops if successful replies covered all needed concrete scopes. Holdings
and choice explanations live only in memory. Renewal still uses issuer routes.

## Consequences

Old identity and session versions are refused; installations reinstall.
The secret provider must accept structured bytes as its existing interface
allows. The file fallback remains plaintext; secure platform stores protect
this material as they protect other secrets.

Hints grant no authority and trusted/local marks change only ranking. A lying
hint still has to pass session authentication. A peer without a root cannot
invent one: it obtains it from a root holder through an admitted session.
Coverage means the peer reported holding the scope, not global freshness or
a guarantee that it retains every operation. Contacts recover devices that
have not caught up with topic changes.

See [enrollment](../runtime/sdk-enrollment.md#peers-and-contacts) and
[phase 14 evidence](../features_07/14-contacts-and-partner-choice-evidence.md).
