# One identity and passkeys

Status: identities, node certificates and the `IdentityProvider` port are
planned in [phase 13](../features_09/13-identities-and-domain-credentials.md);
the passkey module remains an idea.

parings should be not by device, but by identity. A identity = passkey. With the use of PRF (Pseudo-Random Function)
extension we can obtain a deterministic private key. Permissions are given to a identity, this means when I have a
passkey that can be a trusted node, I can spin up trusted nodes. all of these trusted nodes share the same identity, so
when the identity gets revoked all my nodes get revoked automatically as well.

passkeys are not a core feature of rocky, instead they are a first-party module, we don't care how the client stores or
obtains its identity as long as it has the same format as our passkey module
