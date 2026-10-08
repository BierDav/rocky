//! Group secrets: one random secret per group partition and key epoch, the
//! table keys derived from it, and its recipient-bound envelope. See
//! `docs/runtime/partition-credentials.md#group-secrets`.
use crate::{
    Error, PrivateKey, PublicKey, Signature, decode_bounded, denied, encode, enroll::SEAL_OVERHEAD,
    signing_bytes,
};
use curve25519_dalek::{edwards::CompressedEdwardsY, montgomery::MontgomeryPoint};
use rand::RngCore;
use rocky_core::{Commitment, Scope, commitment};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256, Sha512};
use std::collections::BTreeSet;

/// Length of a group secret and of every table key derived from it.
pub const GROUP_SECRET_LEN: usize = 32;
const SEALED_LEN: usize = GROUP_SECRET_LEN + SEAL_OVERHEAD;
const ENVELOPE_VERSION: u32 = 1;
const SIGNED: &str = "rocky.group-secret-envelope.v1";
const CONTEXT: &str = "rocky.group-secret-envelope.context.v1";
const WRAPPING: &str = "rocky.group-secret-envelope.key.v1";
const DERIVATION: &[u8] = b"rocky.group-table-key.v1";

fn hmac(key: &[u8], message: &[&[u8]]) -> [u8; 32] {
    let mut block = [0u8; 64];
    if key.len() > block.len() {
        block[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        block[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(block.map(|byte| byte ^ 0x36));
    for part in message {
        inner.update(part);
    }
    let mut outer = Sha256::new();
    outer.update(block.map(|byte| byte ^ 0x5c));
    outer.update(inner.finalize());
    outer.finalize().into()
}
/// HKDF-SHA256 (RFC 5869) with one 32-byte output block.
fn hkdf(salt: &[u8], secret: &[u8], info: &[u8]) -> [u8; 32] {
    hmac(&hmac(salt, &[secret]), &[info, &[1]])
}

/// The key of one table in one group partition and epoch. HKDF binds the
/// domain, group address, table identity and epoch; a derived key reveals
/// neither the secret nor the key of a sibling table.
pub fn derive_table_key(
    secret: &[u8; GROUP_SECRET_LEN],
    domain: Scope,
    group: Commitment,
    table: Commitment,
    epoch: u64,
) -> Result<[u8; GROUP_SECRET_LEN], Error> {
    if epoch == 0 || epoch > i64::MAX as u64 {
        return Err(denied("invalid group key epoch"));
    }
    let mut info = Vec::with_capacity(32 + 48 + 32 + 8);
    info.extend(group);
    info.extend(domain.mesh.0);
    info.extend(domain.app.0);
    info.extend(domain.domain.0);
    info.extend(table);
    info.extend(epoch.to_be_bytes());
    Ok(hkdf(DERIVATION, secret, &info))
}

/// One group secret wrapped to one recipient, signed by the delivering
/// issuer. Possession establishes no credential right.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupSecretEnvelope {
    body: Body,
    signature: Signature,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    version: u32,
    manifest: Commitment,
    group: Commitment,
    epoch: u64,
    issuer: PublicKey,
    recipient: PublicKey,
    ephemeral: [u8; 32],
    sealed: Vec<u8>,
}
#[derive(Serialize)]
struct Context<'a> {
    version: u32,
    manifest: &'a Commitment,
    group: &'a Commitment,
    epoch: u64,
    issuer: &'a PublicKey,
    recipient: &'a PublicKey,
    ephemeral: &'a [u8; 32],
}
impl Body {
    fn aad(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            CONTEXT,
            &Context {
                version: self.version,
                manifest: &self.manifest,
                group: &self.group,
                epoch: self.epoch,
                issuer: &self.issuer,
                recipient: &self.recipient,
                ephemeral: &self.ephemeral,
            },
        )
    }
    fn wrapping_key(&self, shared: [u8; 32]) -> Result<Commitment, Error> {
        if shared == [0; 32] {
            return Err(denied("invalid group envelope DH key"));
        }
        Ok(commitment(
            WRAPPING,
            &[shared.as_slice(), &self.aad()?].concat(),
        ))
    }
    fn validate(&self) -> Result<(), Error> {
        if self.version != ENVELOPE_VERSION {
            return Err(denied("unsupported group secret envelope version"));
        }
        if self.epoch == 0 || self.epoch > i64::MAX as u64 || self.sealed.len() != SEALED_LEN {
            return Err(denied("invalid group secret envelope metadata"));
        }
        Ok(())
    }
}
impl GroupSecretEnvelope {
    /// Wraps one group epoch to each distinct recipient; refuses an empty set.
    pub fn seal_many(
        issuer: &PrivateKey,
        manifest: Commitment,
        group: Commitment,
        epoch: u64,
        secret: [u8; GROUP_SECRET_LEN],
        recipients: &BTreeSet<PublicKey>,
    ) -> Result<Vec<Self>, Error> {
        if recipients.is_empty() || recipients.len() > 256 {
            return Err(denied("a group envelope needs 1 to 256 recipients"));
        }
        recipients
            .iter()
            .map(|recipient| Self::seal(issuer, manifest, group, epoch, secret, *recipient))
            .collect()
    }
    pub fn seal(
        issuer: &PrivateKey,
        manifest: Commitment,
        group: Commitment,
        epoch: u64,
        secret: [u8; GROUP_SECRET_LEN],
        recipient: PublicKey,
    ) -> Result<Self, Error> {
        let point = CompressedEdwardsY(*recipient.as_bytes())
            .decompress()
            .filter(|point| point.is_torsion_free() && !point.is_small_order())
            .ok_or_else(|| denied("invalid group envelope recipient"))?
            .to_montgomery();
        let mut ephemeral = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut ephemeral);
        let mut body = Body {
            version: ENVELOPE_VERSION,
            manifest,
            group,
            epoch,
            issuer: issuer.public_key(),
            recipient,
            ephemeral: MontgomeryPoint::mul_base_clamped(ephemeral).to_bytes(),
            sealed: Vec::new(),
        };
        let shared = point.mul_clamped(ephemeral).to_bytes();
        body.sealed = crate::enroll::seal(body.wrapping_key(shared)?, &body.aad()?, &secret)?;
        body.validate()?;
        let signature = issuer.sign(&signing_bytes(SIGNED, &body)?);
        Ok(Self { body, signature })
    }
    /// Verifies delivery metadata without possessing the recipient's secret.
    pub fn verify(
        &self,
        issuer: PublicKey,
        manifest: Commitment,
        group: Commitment,
        epoch: u64,
    ) -> Result<(), Error> {
        self.body.validate()?;
        if self.body.issuer != issuer
            || self.body.manifest != manifest
            || self.body.group != group
            || self.body.epoch != epoch
            || !issuer.verify(&signing_bytes(SIGNED, &self.body)?, &self.signature)
        {
            return Err(denied(
                "group envelope issuer, context or signature mismatch",
            ));
        }
        Ok(())
    }
    pub fn open(
        &self,
        recipient: &PrivateKey,
        issuer: PublicKey,
        manifest: Commitment,
        group: Commitment,
        epoch: u64,
    ) -> Result<[u8; GROUP_SECRET_LEN], Error> {
        self.verify(issuer, manifest, group, epoch)?;
        if recipient.public_key() != self.body.recipient {
            return Err(denied("group envelope names another recipient"));
        }
        let digest = Sha512::digest(recipient.as_bytes());
        let mut scalar = [0; 32];
        scalar.copy_from_slice(&digest[..32]);
        let shared = MontgomeryPoint(self.body.ephemeral)
            .mul_clamped(scalar)
            .to_bytes();
        crate::enroll::open(
            self.body.wrapping_key(shared)?,
            &self.body.aad()?,
            &self.body.sealed,
        )?
        .try_into()
        .map_err(|_| denied("invalid group secret length"))
    }
    pub fn recipient(&self) -> PublicKey {
        self.body.recipient
    }
    pub fn group(&self) -> Commitment {
        self.body.group
    }
    pub fn epoch(&self) -> u64 {
        self.body.epoch
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let envelope: Self = decode_bounded(bytes, 4096)?;
        envelope.body.validate()?;
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests;
