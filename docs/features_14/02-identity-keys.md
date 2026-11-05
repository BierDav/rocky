# 2. Identity keys

Status: planned (2026-11-05). Needs phase 1.

Identity keys work like the root: a phrase or a passkey, in memory for one
call.

## Changes

- **`IdentityKey`** in `rocky-credentials`, beside `RootKey`: `generate`,
  `from_secret` (passkey PRF output), `from_phrase`, `phrase`, `secret`,
  `public_key`, and `IdentityProvider`. The phrase and passkey encodings are
  shared with `RootKey`; only the type differs, so an identity key never
  signs as the root.
- **`RootKey::identity()`** derives the owner's identity key from the root
  secret with the label `"rocky identity v1"`.
- **No stored identity.** `identities.rs` `StoredIdentity`, the vault role
  `"identity"` and `Settings.stored_identity` are removed. `Settings::identity`
  returns a provider only when the call supplied one.
- **Single-use identities.** Admission as a new identity generates an
  `IdentityKey`, signs the node certificate and drops it. The settings
  record the identity as `single_use`, and link and unlink calls refuse on
  it.
- **SDK surface.**
  - `Configuration::create` returns the root phrase as today and opens the
    owner's node under the derived identity. `create_with_root` derives it
    from the supplied signer when that signer is a `RootKey`; an
    application signer passes the owner's identity provider explicitly.
  - `Opened.identity: Option<String>` is the phrase of a linkable identity
    the call created; it is `None` for single-use identities and supplied
    providers.
  - Calls needing the identity take it like the root:
    `call_with_identity(request, provider)`, or `"identity": {"phrase": …}`
    / `{"secret": …}` in the request.

## Done when

- A linkable identity's phrase and its passkey-derived secret sign the same
  certificates as before the restart.
- No identity secret is found in the secret store or settings file after
  create, join, link or renewal (test scans both).
- The owner of a fresh database restores both root and owner identity from
  one phrase.
