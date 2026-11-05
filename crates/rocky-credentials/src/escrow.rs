//! Root escrow: a data key sealed to the domain's offline root, so the root
//! alone recovers what its devices could read. Sealing needs only the
//! root's public key, which every device pins as the publisher.
//!
//! The recipient key is the root's Ed25519 key mapped to X25519 (the
//! birational map; its secret scalar is the clamped first half of
//! SHA-512 of the root secret), as libsodium's `ed25519_*_to_curve25519`
//! derive it. A record is an ECIES sealed box: a fresh X25519 ephemeral,
//! a wrapping key hashed from the shared secret and the record's context,
//! and ChaCha20-Poly1305 over the padded payload. Records carry no scope,
//! partition or collection identifier: only the tag, a hash of the sealed
//! secret that only its holders can compute, so a holder skips escrowing
//! a key twice and nobody else can link a record to anything.
use crate::{
    CredentialChain, Error, PrivateKey, PublicKey, RootKey, Signature, decode_bounded, denied,
    encode,
    group::{agree, agreed},
    signing_bytes,
};
use rocky_core::{Commitment, KeyEpoch, ResourceScope, commitment};
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;
const CONTEXT: &str = "rocky.root-escrow.context.v1";
const WRAPPING: &str = "rocky.root-escrow.key.v1";
const TAG: &str = "rocky.root-escrow.tag.v1";
const VOUCH: &str = "rocky.root-escrow.vouch.v1";
/// A vouched record's bound: the record and its signer's chain.
const MAX_VOUCHED_BYTES: usize = MAX_RECORD_BYTES + 64 * 1024;
/// Payloads are padded to a multiple of this, hiding their exact length.
const PADDING: usize = 256;
/// The largest payload one record seals.
pub const MAX_ESCROW_PAYLOAD: usize = 16 * 1024;
const MAX_RECORD_BYTES: usize = MAX_ESCROW_PAYLOAD + 2 * PADDING;

/// The tag of the record that escrows `secret` to `root`.
pub fn escrow_tag(root: PublicKey, secret: &[u8; 32]) -> Commitment {
    commitment(TAG, &[root.as_bytes().as_slice(), secret].concat())
}

/// One payload sealed to the root's public key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootEscrow {
    version: u32,
    tag: Commitment,
    ephemeral: [u8; 32],
    #[serde(with = "serde_bytes")]
    sealed: Vec<u8>,
}
#[derive(Serialize)]
struct Context<'a> {
    version: u32,
    root: &'a PublicKey,
    tag: &'a Commitment,
    ephemeral: &'a [u8; 32],
}
impl RootEscrow {
    /// Seals `payload` to `root` under `tag`, the `escrow_tag` of the key
    /// the payload carries.
    pub fn seal(root: PublicKey, tag: Commitment, payload: &[u8]) -> Result<Self, Error> {
        if payload.len() > MAX_ESCROW_PAYLOAD {
            return Err(denied("escrow payload quota"));
        }
        let (ephemeral, shared) = agree(root)?;
        let mut record = Self {
            version: VERSION,
            tag,
            ephemeral,
            sealed: Vec::new(),
        };
        let padded = payload.len().div_ceil(PADDING).max(1) * PADDING;
        let mut plaintext = Vec::with_capacity(4 + padded);
        plaintext.extend(
            u32::try_from(payload.len())
                .map_err(|_| denied("escrow payload quota"))?
                .to_be_bytes(),
        );
        plaintext.extend(payload);
        plaintext.resize(4 + padded, 0);
        let (key, aad) = record.keys(root, shared)?;
        record.sealed = crate::enroll::seal(key, &aad, &plaintext)?;
        Ok(record)
    }
    /// The payload, for the root this record was sealed to; any other key
    /// fails.
    pub fn open(&self, root: &RootKey) -> Result<Vec<u8>, Error> {
        self.validate()?;
        let shared = agreed(root.private(), self.ephemeral);
        let (key, aad) = self.keys(root.public_key(), shared)?;
        let plaintext = crate::enroll::open(key, &aad, &self.sealed)
            .map_err(|_| denied("the escrow record is not sealed to this root"))?;
        let (length, rest) = plaintext
            .split_at_checked(4)
            .ok_or_else(|| denied("invalid escrow padding"))?;
        let length = u32::from_be_bytes(
            length
                .try_into()
                .map_err(|_| denied("invalid escrow padding"))?,
        ) as usize;
        rest.get(..length)
            .filter(|_| rest[length..].iter().all(|byte| *byte == 0))
            .map(<[u8]>::to_vec)
            .ok_or_else(|| denied("invalid escrow padding"))
    }
    fn keys(&self, root: PublicKey, shared: [u8; 32]) -> Result<(Commitment, Vec<u8>), Error> {
        if shared == [0; 32] {
            return Err(denied("invalid escrow DH key"));
        }
        let aad = signing_bytes(
            CONTEXT,
            &Context {
                version: self.version,
                root: &root,
                tag: &self.tag,
                ephemeral: &self.ephemeral,
            },
        )?;
        Ok((
            commitment(WRAPPING, &[shared.as_slice(), &aad].concat()),
            aad,
        ))
    }
    fn validate(&self) -> Result<(), Error> {
        if self.version != VERSION {
            return Err(denied("unsupported root escrow version"));
        }
        if self.sealed.len() > MAX_RECORD_BYTES
            || self.sealed.len() < crate::enroll::SEAL_OVERHEAD + 4 + PADDING
        {
            return Err(denied("invalid root escrow length"));
        }
        Ok(())
    }
    pub fn tag(&self) -> Commitment {
        self.tag
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let record: Self = decode_bounded(bytes, MAX_RECORD_BYTES + 256)?;
        record.validate()?;
        Ok(record)
    }
}

/// A record as the node that sealed it vouches for it: signed with the
/// scope and key epoch whose frames carry it, at `signed_at`, under the
/// chain that let it read them. Anyone can seal to the root, so a
/// receiver keeps a record only when a key holder vouches for it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VouchedEscrow {
    body: Vouch,
    signature: Signature,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Vouch {
    version: u32,
    scope: ResourceScope,
    epoch: KeyEpoch,
    signer: PublicKey,
    signed_at: u64,
    chain: CredentialChain,
    record: RootEscrow,
}
impl VouchedEscrow {
    pub fn sign(
        node: &PrivateKey,
        chain: CredentialChain,
        (scope, epoch): (ResourceScope, KeyEpoch),
        record: RootEscrow,
        signed_at: u64,
    ) -> Result<Self, Error> {
        let body = Vouch {
            version: VERSION,
            scope,
            epoch,
            signer: node.public_key(),
            signed_at,
            chain,
            record,
        };
        let signature = node.sign(&signing_bytes(VOUCH, &body)?);
        Ok(Self { body, signature })
    }
    /// Checks the signer's signature; the caller judges its chain.
    pub fn verify(&self) -> Result<(), Error> {
        if self.body.version != VERSION
            || !self
                .body
                .signer
                .verify(&signing_bytes(VOUCH, &self.body)?, &self.signature)
        {
            return Err(denied("escrow vouch signature mismatch"));
        }
        self.body.record.validate()
    }
    pub fn slot(&self) -> (ResourceScope, KeyEpoch) {
        (self.body.scope, self.body.epoch)
    }
    pub fn signer(&self) -> PublicKey {
        self.body.signer
    }
    pub fn signed_at(&self) -> u64 {
        self.body.signed_at
    }
    pub fn chain(&self) -> &CredentialChain {
        &self.body.chain
    }
    pub fn record(&self) -> &RootEscrow {
        &self.body.record
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, MAX_VOUCHED_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrivateKey;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn only_the_root_opens_a_record_and_it_names_nothing() -> Result<(), Error> {
        let root = RootKey::generate();
        let secret = [7; 32];
        let tag = escrow_tag(root.public_key(), &secret);
        let payload = b"partition acme/invoices and its secret".to_vec();
        let record = RootEscrow::seal(root.public_key(), tag, &payload)?;
        let bytes = record.to_bytes()?;
        let parsed = RootEscrow::from_bytes(&bytes)?;
        assert_eq!(
            parsed.open(&RootKey::from_phrase(&root.phrase())?)?,
            payload
        );
        assert_eq!(parsed.tag(), tag);
        // The record carries neither the payload nor the secret in clear.
        assert!(!bytes.windows(5).any(|window| window == b"acme/"));
        assert!(!bytes.windows(32).any(|window| window == secret));
        // Lengths are padded: a longer payload of the same bucket looks alike.
        let longer = RootEscrow::seal(root.public_key(), tag, &[1; 200])?;
        assert_eq!(longer.sealed.len(), parsed.sealed.len());
        // Another root, or a device key, opens nothing.
        assert!(parsed.open(&RootKey::generate()).is_err());
        let device = PrivateKey::from_bytes(&[9; 32]);
        assert!(
            parsed
                .open(&RootKey::from_secret(*device.as_bytes()))
                .is_err()
        );
        // A changed tag or byte fails authentication.
        let mut forged = parsed.clone();
        forged.tag = escrow_tag(root.public_key(), &[8; 32]);
        assert!(forged.open(&root).is_err());
        let mut flipped = parsed.clone();
        flipped.sealed[20] ^= 1;
        assert!(flipped.open(&root).is_err());
        // Tags depend on the secret and the root.
        assert_ne!(escrow_tag(RootKey::generate().public_key(), &secret), tag);
        Ok(())
    }
}
