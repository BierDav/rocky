# 5. Admission and discovery

Status: Phase 5a implemented; workspace gate limitations recorded (2026-11-06), based on
main `cec2ad1d78393e4ebee6366fefa6dda9f44df8ed` (phases 1, 2a and 4).
[Phase 5a evidence](05-admission-and-discovery-evidence.md) records checks and
remaining scope. Phase 3 membership remains a prerequisite for linking and
the later discovery work.

## Phase 5a

Approval invitations, pending requests, code-confirmed `admit` and `refuse`
are implemented. Requests survive issuer restarts, retain their keys and nonce
on retry, expire with the invitation, and evict the oldest above 32 pending.
Pre-approved `invitation_with(profile)` still enrolls immediately. Admission
writes identity-keyed registry rows and labels under node-signed issuer
authority before returning the enrollment. Retries reuse their author range
and refuse a revoked node or identity.

The settings format is v29, invitations v6, enrollment answers v4 and pairing
requests v4; each accepts only its current format. Custody routing preserves
the original issuer node used by the code. `discovered` currently lists only
pending requests. Certified seen-node discovery, membership linking,
linkable/single-use identity creation and Paperless integration are not part
of Phase 5a; the remaining sections describe the full phase destination.

## Discovered devices

`discovered` (any node, not only issuers) lists:

- **Pending**: join requests this node received and has not answered:
  node key, nonce, invitation digest, self-reported name, received time,
  code. Issuer nodes only. At most 32, each expiring with its invitation;
  the oldest is evicted at the bound.
- **Seen**: nodes met through discovery topics, `peers` or sync, with the
  identity that certified them, last seen, and whether that identity is
  known to the registry, revoked or unlinked. A certified node of an
  identity the viewer cannot place is flagged.

## Invitations

- `Session::invitation()` carries no profile. Answering it makes a pending
  request. The joining device's `join` returns `AwaitingAdmission`
  (retryable, same keys and nonce) until an issuer admits it, then
  enrolls.
- `Session::invitation_with(profile)` keeps today's behavior: the request
  is admitted on arrival as a new single-use identity.
- `Session::pairing(profile)` is removed.

## Admit

`admit { node, code, profile, label, linkable }`, issuer only:

- Refused unless `code` equals the pending request's code: 6 digits from a
  hash of issuer node, requesting node and nonce, also shown on the
  joining device.
- Creates the identity. Single-use by default: the joining node generates
  the identity key, signs its own certificate and discards the key; the
  issuer's capability names that identity. With `linkable`, the joining
  device shows the new phrase or binds a passkey before it signs.
- Records the identity, answers the pending request and removes it.

`refuse { node }` drops a pending request.

## Link from the new device

Phase 3's link flow, reached from `join` with an identity instead of an
invitation: `Configuration::link(contact, identity_provider)`. Both devices
show the code before the bundle is sent.

## Done when

- A device joining with `invitation()` stays pending until admitted with
  the right code, and is refused with a wrong one.
- 33 pending requests keep the newest 32.
- Paperless's owner-approval join (`native/join_approval.rs`) is replaced
  by `invitation()` + `admit`.
