//! Envelope delivery: a payload sealed to one recipient's node key, which a
//! mailbox holds without learning what it carries, and the recipient's
//! signed receipt. See `docs/decisions/0054-envelope-delivery.md`.
use crate::{
    Error, PrivateKey, PublicKey, Signature, decode_bounded, denied, encode,
    enroll::SEAL_OVERHEAD,
    group::{agree, agreed},
    signing_bytes,
};
use rocky_core::{Commitment, commitment};
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;
const CONTEXT: &str = "rocky.envelope-delivery.context.v1";
const WRAPPING: &str = "rocky.envelope-delivery.key.v1";
const DIGEST: &str = "rocky.envelope-delivery.digest.v1";
const RECEIPT: &str = "rocky.envelope-receipt.v1";
/// Largest payload one delivery seals: it and its wrapping fit one session
/// message.
pub const MAX_DELIVERY_PAYLOAD: usize = 112 * 1024;
const MAX_DELIVERY_BYTES: usize = MAX_DELIVERY_PAYLOAD + SEAL_OVERHEAD + 256;
const RECEIPT_BYTES: usize = 256;

/// A payload sealed to one recipient. Only the version, the recipient's node
/// key and fresh ephemeral material are in the clear.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedDelivery {
    version: u32,
    recipient: PublicKey,
    ephemeral: [u8; 32],
    #[serde(with = "serde_bytes")]
    sealed: Vec<u8>,
}
#[derive(Serialize)]
struct Context<'a> {
    version: u32,
    recipient: &'a PublicKey,
    ephemeral: &'a [u8; 32],
}
impl SealedDelivery {
    /// Seals `payload` to `recipient` with fresh ephemeral material, so two
    /// sealings of one payload share nothing a mailbox could compare.
    pub fn seal(recipient: PublicKey, payload: &[u8]) -> Result<Self, Error> {
        if payload.is_empty() || payload.len() > MAX_DELIVERY_PAYLOAD {
            return Err(denied("delivery payload outside its bound"));
        }
        let (ephemeral, shared) = agree(recipient)?;
        let mut delivery = Self {
            version: VERSION,
            recipient,
            ephemeral,
            sealed: Vec::new(),
        };
        delivery.sealed = crate::enroll::seal(delivery.key(shared)?, &delivery.aad()?, payload)?;
        Ok(delivery)
    }
    /// The payload, for the recipient's node key only.
    pub fn open(&self, recipient: &PrivateKey) -> Result<Vec<u8>, Error> {
        if recipient.public_key() != self.recipient {
            return Err(denied("the delivery names another recipient"));
        }
        let shared = agreed(recipient, self.ephemeral);
        crate::enroll::open(self.key(shared)?, &self.aad()?, &self.sealed)
    }
    fn aad(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            CONTEXT,
            &Context {
                version: self.version,
                recipient: &self.recipient,
                ephemeral: &self.ephemeral,
            },
        )
    }
    fn key(&self, shared: [u8; 32]) -> Result<Commitment, Error> {
        if shared == [0; 32] {
            return Err(denied("invalid delivery DH key"));
        }
        Ok(commitment(
            WRAPPING,
            &[shared.as_slice(), &self.aad()?].concat(),
        ))
    }
    pub fn recipient(&self) -> PublicKey {
        self.recipient
    }
    /// The sealed payload's length, which a mailbox bounds.
    pub fn payload_len(&self) -> usize {
        self.sealed.len().saturating_sub(SEAL_OVERHEAD)
    }
    /// What a receipt signs: the same bytes reach the recipient directly
    /// and through every mailbox.
    pub fn digest(&self) -> Result<Commitment, Error> {
        Ok(commitment(DIGEST, &self.to_bytes()?))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let delivery: Self = decode_bounded(bytes, MAX_DELIVERY_BYTES)?;
        if delivery.version != VERSION {
            return Err(denied("unsupported envelope delivery version"));
        }
        if delivery.sealed.len() <= SEAL_OVERHEAD || delivery.payload_len() > MAX_DELIVERY_PAYLOAD {
            return Err(denied("delivery payload outside its bound"));
        }
        Ok(delivery)
    }
}

/// The recipient's acknowledgment of one delivery, signed by its node key
/// over the delivery's digest. A mailbox keeps it in place of the delivery.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeliveryReceipt {
    body: ReceiptBody,
    signature: Signature,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptBody {
    version: u32,
    recipient: PublicKey,
    digest: Commitment,
}
impl DeliveryReceipt {
    pub fn sign(recipient: &PrivateKey, digest: Commitment) -> Result<Self, Error> {
        let body = ReceiptBody {
            version: VERSION,
            recipient: recipient.public_key(),
            digest,
        };
        let signature = recipient.sign(&signing_bytes(RECEIPT, &body)?);
        Ok(Self { body, signature })
    }
    /// Checks that `recipient` acknowledged the delivery of `digest`.
    pub fn verify(&self, recipient: PublicKey, digest: Commitment) -> Result<(), Error> {
        if self.body.version != VERSION
            || self.body.recipient != recipient
            || self.body.digest != digest
            || !recipient.verify(&signing_bytes(RECEIPT, &self.body)?, &self.signature)
        {
            return Err(denied(
                "delivery receipt recipient, digest or signature mismatch",
            ));
        }
        Ok(())
    }
    pub fn recipient(&self) -> PublicKey {
        self.body.recipient
    }
    pub fn digest(&self) -> Commitment {
        self.body.digest
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    /// Decodes a receipt; `verify` checks it.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, RECEIPT_BYTES)
    }
}

#[cfg(test)]
mod tests;
