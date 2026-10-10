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
pub const MAX_GROUP_ENVELOPE_BYTES: usize = 128 * 1024;
const SEALED_LEN: usize = GROUP_SECRET_LEN + SEAL_OVERHEAD;
const ENVELOPE_VERSION: u32 = 2;
const SIGNED: &str = "rocky.group-secret-envelope.v2";
const CONTEXT: &str = "rocky.group-secret-envelope.context.v2";
const WRAPPING: &str = "rocky.group-secret-envelope.key.v2";
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

/// A fresh ephemeral X25519 public key and the secret it shares with the
/// recipient's Ed25519 key, converted to X25519.
pub(crate) fn agree(recipient: PublicKey) -> Result<([u8; 32], [u8; 32]), Error> {
    let point = CompressedEdwardsY(*recipient.as_bytes())
        .decompress()
        .filter(|point| point.is_torsion_free() && !point.is_small_order())
        .ok_or_else(|| denied("invalid envelope recipient"))?
        .to_montgomery();
    let mut ephemeral = [0; 32];
    rand::rngs::OsRng.fill_bytes(&mut ephemeral);
    Ok((
        MontgomeryPoint::mul_base_clamped(ephemeral).to_bytes(),
        point.mul_clamped(ephemeral).to_bytes(),
    ))
}
/// The secret `agree` shared, on the recipient's side.
pub(crate) fn agreed(recipient: &PrivateKey, ephemeral: [u8; 32]) -> [u8; 32] {
    let digest = Sha512::digest(recipient.as_bytes());
    let mut scalar = [0; 32];
    scalar.copy_from_slice(&digest[..32]);
    MontgomeryPoint(ephemeral).mul_clamped(scalar).to_bytes()
}

/// One group secret wrapped to one recipient, signed by the delivering
/// signer. Possession establishes no credential right.
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
    epoch: rocky_core::KeyEpoch,
    statements: Vec<Commitment>,
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
    epoch: rocky_core::KeyEpoch,
    statements: Vec<Commitment>,
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
                statements: self.statements.clone(),
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
        if self.statements.len() > crate::MAX_LIVE_REVOCATIONS
            || self.statements.windows(2).any(|pair| pair[0] >= pair[1])
        {
            return Err(denied("invalid group epoch revocation IDs"));
        }
        if !self.epoch.valid() || self.sealed.len() != SEALED_LEN {
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
        Self::seal_recorded(
            issuer,
            manifest,
            group,
            epoch,
            secret,
            recipient,
            Vec::new(),
        )
    }
    pub fn seal_recorded(
        issuer: &PrivateKey,
        manifest: Commitment,
        group: Commitment,
        epoch: u64,
        secret: [u8; GROUP_SECRET_LEN],
        recipient: PublicKey,
        statements: Vec<Commitment>,
    ) -> Result<Self, Error> {
        let (ephemeral, shared) = agree(recipient)?;
        let mut body = Body {
            version: ENVELOPE_VERSION,
            manifest,
            group,
            epoch: rocky_core::KeyEpoch::new(epoch, &secret),
            statements,
            issuer: issuer.public_key(),
            recipient,
            ephemeral,
            sealed: Vec::new(),
        };
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
        epoch: rocky_core::KeyEpoch,
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
        epoch: rocky_core::KeyEpoch,
    ) -> Result<[u8; GROUP_SECRET_LEN], Error> {
        self.verify(issuer, manifest, group, epoch)?;
        if recipient.public_key() != self.body.recipient {
            return Err(denied("group envelope names another recipient"));
        }
        let shared = agreed(recipient, self.body.ephemeral);
        let secret = crate::enroll::open(
            self.body.wrapping_key(shared)?,
            &self.body.aad()?,
            &self.body.sealed,
        )?
        .try_into()
        .map_err(|_| denied("invalid group secret length"))?;
        if rocky_core::KeyEpoch::new(epoch.generation, &secret) != epoch {
            return Err(denied("group secret epoch identity mismatch"));
        }
        Ok(secret)
    }
    pub fn statements(&self) -> &[Commitment] {
        &self.body.statements
    }
    pub fn issuer(&self) -> PublicKey {
        self.body.issuer
    }
    pub fn recipient(&self) -> PublicKey {
        self.body.recipient
    }
    pub fn group(&self) -> Commitment {
        self.body.group
    }
    pub fn epoch(&self) -> rocky_core::KeyEpoch {
        self.body.epoch
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_GROUP_ENVELOPE_BYTES {
            return Err(denied("group envelope byte capacity reached"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let envelope: Self = decode_bounded(bytes, MAX_GROUP_ENVELOPE_BYTES)?;
        envelope.body.validate()?;
        Ok(envelope)
    }
}

#[cfg(test)]
mod tests;
