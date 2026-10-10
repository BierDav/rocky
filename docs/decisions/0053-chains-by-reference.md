# 0053: Credential chains by reference

Status: accepted.
Date: 2026-10-10.

## Context

Every encrypted frame carried its author's credential chain inside the signed
header: 32 KB with 20 scope grants, beside a 1.2 KB frame. A page held three
such frames, a replica stored the chain once per operation, and every header
was encoded, hashed and signed over it. A checkpoint's first page embedded the
latest confirmed command with the caller's and the executor's chains: 79 KB
for two 20-grant chains, more than the 63 KiB the sealed page may hold, so the
scope could not bootstrap.

[Phase 3](../features_07/03-catch-up-cost.md) profiled the four-table fixture.
After its fixes, chain bytes (encoding, hashing, signing over, transferring
and storing chains) were 10% of register and author, 8% of catch-up and 48% of
the checkpoint stage. The [brief](../features_07/11-chains-by-reference.md)
does every slice when one stage reaches 20%; the checkpoint stage does.

## Decision

**Digest.** A chain's digest is `commitment("rocky.credential-chain.v1", bytes)`
over its canonical CBOR. It names bytes and nothing else: every frame, request
and command still verifies its chain against the current revocations. No
verification result is cached by digest.

**Frame.** Row format 8 replaces 7. `Extensions.chain` holds the digest where
`credentials` held the chain; the operation hash and signature cover it. An
`EncryptedFrame` is the header, the body and, beside them and unsigned, the
chain. `to_bytes` writes all three for a transport outside a session. A frame
without the chain its header names is incomplete: `receive` refuses it and
stores nothing. A frame's weight in a page and in the retention bounds is its
header and body.

**Session.** Native session version 11 replaces 10 and adds the `chains`
module. Before a `Page`, `RangeFrames` or `Push` whose frames name a chain the
session has not carried, the sender sends `Chains::Carry` with it, in messages
of at most 128 KiB of chains, then the page with bare frames. A page therefore
keeps its whole quota for frames. `Carry` takes no answer. Each side keeps the
digests it sent and the chains it received for the session only; the session's
byte budget bounds them. A received frame is completed from that table alone,
never from stored chains or one sent inside the page. A frame whose chain the
session did not carry is not received or acknowledged; `CatchUp::incomplete`
and `Pulled::incomplete` count such frames, and a pass on another session
receives them.

**Storage.** `rocky.encrypted-replica.v11` replaces v10. `secure_chains` holds
each chain once by digest. `secure_operations.chain` and
`secure_horizon_credentials.chain` name it; a registered row author's chain is
stored at registration. An operation is inserted only when its chain is
stored, in the same transaction. The transaction that prunes, evicts or
supersedes operations, or replaces a horizon credential, removes every chain
that no operation, horizon credential or registered row author names. Chain
bytes do not count toward `RetentionLimits`.

**Checkpoint page.** The first page's strong state names the latest command's
chains by digest and omits the importer's own checkpoint. The chains travel
beside the page: `CheckpointPage::chains` in memory, one `CheckpointChain`
message each before `CheckpointRows` on a session. `stage_checkpoint_page`
rebuilds the command and refuses the page, staging nothing, unless exactly the
named chains are present. `EncryptedCommand`, `ConfirmedCommand`, their
signatures and `rocky/command/1` are unchanged. Row records, command byte
strings and the page's ciphertext are CBOR byte strings, so the row budget is
the 63 KiB plaintext, no longer half of it.

## Consequences

- A page holds 32 frames of a 20-grant author where it held 3. The first
  page's strong state is 3.1 KB for 20-grant chains where it was 79 KB.
- A chain still crosses every session that carries its frames, in the clear
  to the peer, as the handshake's chains do.
- Recovery packages and `to_bytes` keep one chain per frame.
- Signed checkpoints, existence records and horizon statements still embed
  their signer's chain.
- A confirmed command must itself fit 128 KiB with both chains, which two
  40-grant chains of full rights do not; that bound is not changed here.
- Existing replicas and peers on session 10 are refused; installations
  reinstall ([decision 0039](0039-single-current-formats.md)).

[Secure sessions](../runtime/secure-sessions.md#native-peer-sessions),
[encrypted checkpoints](../runtime/encrypted-checkpoints.md#rebootstrap) and
[replica roles](../runtime/replica-roles.md#chains-by-reference) describe the
behavior; the [partition guide](../runtime/partitioned-scopes.md#four-table-profile)
holds the measurements.
