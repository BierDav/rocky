//! Author pseudonyms and author ranges: the versioned capability extension,
//! the device sealed to the publisher and the request a device sends for a
//! pseudonym. See `docs/runtime/credential-primitives.md`.
use crate::{
    CredentialChain, Error, MAX_CREDENTIAL_BYTES, PrivateKey, PublicKey, RevocationSet,
    decode_bounded, denied, encode, signing_bytes,
};
use curve25519_dalek::{edwards::CompressedEdwardsY, montgomery::MontgomeryPoint};
use p2panda_core::Signature;
use rand::RngCore;
use rocky_core::{Commitment, ResourceGrant, Right, commitment};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha512};

/// The highest author number: the `id` keeps 31 bits for it.
pub const MAX_AUTHOR: u32 = (1 << 31) - 1;
const EXTENSION_VERSION: u32 = 2;
const MAX_EXTENSION: usize = 64 * 1024;

/// Author numbers `first..=last`. Author 0 is reserved for the virtual row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorRange {
    pub first: u32,
    pub last: u32,
}
impl AuthorRange {
    /// Every assignable author number.
    pub const ALL: Self = Self {
        first: 1,
        last: MAX_AUTHOR,
    };
    pub fn new(first: u32, last: u32) -> Result<Self, Error> {
        let range = Self { first, last };
        if !range.valid() {
            return Err(denied("an author range is 1 to 2^31-1 and not empty"));
        }
        Ok(range)
    }
    fn valid(&self) -> bool {
        1 <= self.first && self.first <= self.last && self.last <= MAX_AUTHOR
    }
    pub fn contains(&self, author: u32) -> bool {
        (self.first..=self.last).contains(&author)
    }
    pub fn includes(&self, other: &Self) -> bool {
        self.first <= other.first && other.last <= self.last
    }
    /// A pseudonym's own number, when the range is exactly one.
    pub fn single(&self) -> Option<u32> {
        (self.first == self.last).then_some(self.first)
    }
}

/// The node key of a pseudonym's device, sealed to the publisher key.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SealedDevice {
    ephemeral: [u8; 32],
    sealed: Vec<u8>,
}
const DEVICE_LABEL: &[u8] = b"rocky.credentials.pseudonym-device.v1";
impl SealedDevice {
    fn key(
        shared: &[u8; 32],
        ephemeral: &[u8; 32],
        publisher: &PublicKey,
        pseudonym: &PublicKey,
        author: u32,
    ) -> Commitment {
        commitment(
            "rocky.credentials.pseudonym-device-key.v1",
            &[
                shared.as_slice(),
                ephemeral,
                publisher.as_bytes(),
                pseudonym.as_bytes(),
                &author.to_be_bytes(),
            ]
            .concat(),
        )
    }
    /// Seals `device` so that only a holder of `publisher`'s secret opens it.
    pub fn seal(
        publisher: PublicKey,
        pseudonym: PublicKey,
        author: u32,
        device: PublicKey,
    ) -> Result<Self, Error> {
        let mut secret = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        let ephemeral = MontgomeryPoint::mul_base_clamped(secret).to_bytes();
        let shared = montgomery(&publisher)?.mul_clamped(secret).to_bytes();
        if shared == [0; 32] {
            return Err(denied("invalid publisher key"));
        }
        let key = Self::key(&shared, &ephemeral, &publisher, &pseudonym, author);
        Ok(Self {
            ephemeral,
            sealed: crate::enroll::seal(key, DEVICE_LABEL, device.as_bytes())?,
        })
    }
    /// The device, for a holder of the publisher's secret key.
    pub fn open(
        &self,
        publisher: &PrivateKey,
        pseudonym: PublicKey,
        author: u32,
    ) -> Result<PublicKey, Error> {
        let digest = Sha512::digest(publisher.as_bytes());
        let mut scalar = [0; 32];
        scalar.copy_from_slice(&digest[..32]);
        let shared = MontgomeryPoint(self.ephemeral)
            .mul_clamped(scalar)
            .to_bytes();
        let key = Self::key(
            &shared,
            &self.ephemeral,
            &publisher.public_key(),
            &pseudonym,
            author,
        );
        let device = crate::enroll::open(key, DEVICE_LABEL, &self.sealed)
            .map_err(|_| denied("only the domain's issuers open a pseudonym's device"))?;
        let device: [u8; 32] = device
            .try_into()
            .map_err(|_| denied("invalid sealed device"))?;
        PublicKey::from_bytes(&device).map_err(|_| denied("invalid sealed device"))
    }
}
/// An Ed25519 public key as the X25519 point of the same secret.
fn montgomery(key: &PublicKey) -> Result<MontgomeryPoint, Error> {
    CompressedEdwardsY(*key.as_bytes())
        .decompress()
        .map(|point| point.to_montgomery())
        .ok_or_else(|| denied("invalid publisher key"))
}

/// A capability's required, canonical versioned authority fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extension(Vec<u8>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    version: u32,
    authors: Option<AuthorRange>,
    device: Option<SealedDevice>,
    partitions: Vec<crate::PartitionGrant>,
}
/// Reads only the version, whatever else a later body holds.
#[derive(Deserialize)]
struct Versioned {
    version: u32,
}
/// What an extension carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extended {
    pub authors: Option<AuthorRange>,
    pub device: Option<SealedDevice>,
    /// Selector grants (extension version 2).
    pub partitions: Vec<crate::PartitionGrant>,
}
impl Extension {
    pub fn new(extended: Extended) -> Result<Self, Error> {
        let body = Body {
            version: EXTENSION_VERSION,
            partitions: extended.partitions,
            authors: extended.authors,
            device: extended.device,
        };
        let extension = Self(encode(&body)?);
        extension.read()?;
        Ok(extension)
    }
    /// The body, refusing a version this runtime does not know.
    pub fn read(&self) -> Result<Extended, Error> {
        if self.0.len() > MAX_EXTENSION {
            return Err(denied("credential extension quota"));
        }
        let versioned: Versioned = p2panda_core::cbor::decode_cbor(self.0.as_slice())
            .map_err(|_| denied("invalid credential extension"))?;
        if versioned.version != EXTENSION_VERSION {
            return Err(denied("unsupported credential extension version"));
        }
        let body: Body = decode_bounded(&self.0, MAX_EXTENSION)?;
        if body.authors.is_some_and(|authors| !authors.valid()) {
            return Err(denied("invalid credential extension"));
        }
        if body.partitions.len() > crate::MAX_GRANTS {
            return Err(denied("invalid partition credential extension"));
        }
        for grant in &body.partitions {
            grant.validate()?;
        }
        Ok(Extended {
            partitions: body.partitions,
            authors: body.authors,
            device: body.device,
        })
    }
}

/// What a verified pseudonym chain establishes.
#[derive(Clone, Debug)]
pub struct Pseudonym {
    /// The pseudonym's signing key.
    pub key: PublicKey,
    pub author: u32,
    /// The table its grants name.
    pub table: Commitment,
    pub grants: Vec<ResourceGrant>,
    /// The key that assigned the number: an issuer's or a delegate's user.
    pub assigner: PublicKey,
    pub expires: u64,
}

/// A device asks whoever assigns author numbers for a pseudonym: a key for
/// one table, proven by its node key and the pseudonym key both.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PseudonymRequest {
    version: u32,
    pub node: PublicKey,
    pub chain: CredentialChain,
    pub key: PublicKey,
    pub table: Commitment,
    time: u64,
    /// A pseudonym this key held for the table, whose number it keeps.
    pub previous: Option<CredentialChain>,
    node_signature: Signature,
    key_signature: Signature,
}
#[derive(Serialize)]
struct RequestBody<'a> {
    version: u32,
    node: &'a PublicKey,
    chain: &'a CredentialChain,
    key: &'a PublicKey,
    table: &'a Commitment,
    time: u64,
    previous: &'a Option<CredentialChain>,
}
impl PseudonymRequest {
    const VERSION: u32 = 1;
    fn body(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.pseudonym-request.v1",
            &RequestBody {
                version: self.version,
                node: &self.node,
                chain: &self.chain,
                key: &self.key,
                table: &self.table,
                time: self.time,
                previous: &self.previous,
            },
        )
    }
    pub fn sign(
        node: &PrivateKey,
        key: &PrivateKey,
        chain: CredentialChain,
        table: Commitment,
        previous: Option<CredentialChain>,
        time: u64,
    ) -> Result<Self, Error> {
        let mut request = Self {
            version: Self::VERSION,
            node: node.public_key(),
            chain,
            key: key.public_key(),
            table,
            time,
            previous,
            // Replaced below; a signature has no neutral value.
            node_signature: node.sign(&[]),
            key_signature: key.sign(&[]),
        };
        let body = request.body()?;
        request.node_signature = node.sign(&body);
        request.key_signature = key.sign(&body);
        Ok(request)
    }
    /// The grants the pseudonym receives: the device's write grants on the
    /// table, from its node chain under `revocations` at `time`.
    pub fn verify_selectors(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        revocations: &RevocationSet,
        (time, skew): (u64, u64),
    ) -> Result<(Vec<ResourceGrant>, Vec<crate::PartitionGrant>), Error> {
        if self.version != Self::VERSION {
            return Err(Error::Denied(format!(
                "pseudonym request version {} is unsupported",
                self.version
            )));
        }
        let body = self.body()?;
        if self.time.abs_diff(time) > skew
            || self.key == self.node
            || !self.node.verify(&body, &self.node_signature)
            || !self.key.verify(&body, &self.key_signature)
        {
            return Err(denied("invalid pseudonym request"));
        }
        let (grants, partitions) =
            self.chain
                .verify_selectors(publisher, manifest, self.node, revocations, time)?;
        let writes = |rights: &std::collections::BTreeSet<Right>| {
            [Right::Insert, Right::Update, Right::Delete]
                .iter()
                .any(|right| rights.contains(right))
        };
        let grants: Vec<_> = grants
            .into_iter()
            .filter(|grant| grant.scope.table == self.table && writes(&grant.rights))
            .collect();
        let partitions: Vec<_> = partitions
            .into_iter()
            .filter(|grant| grant.table == self.table && writes(&grant.rights))
            .collect();
        if grants.is_empty() && partitions.is_empty() {
            return Err(denied("the device writes nothing in this table"));
        }
        Ok((grants, partitions))
    }
    pub fn verify(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        revocations: &RevocationSet,
        validity: (u64, u64),
    ) -> Result<Vec<ResourceGrant>, Error> {
        let (grants, partitions) =
            self.verify_selectors(publisher, manifest, revocations, validity)?;
        if !partitions.is_empty() {
            return Err(denied(
                "partition pseudonym request requires selector verification",
            ));
        }
        Ok(grants)
    }
    /// The number an earlier pseudonym of this key and table held, if it
    /// lies in `range`: it keeps that number.
    pub fn previous_author(
        &self,
        publisher: PublicKey,
        manifest: Commitment,
        range: AuthorRange,
    ) -> Option<u32> {
        let previous = self.previous.as_ref()?;
        let leaf = previous.pseudonym_leaf(publisher, manifest).ok()?;
        (leaf.key == self.key && leaf.table == self.table && range.contains(leaf.author))
            .then_some(leaf.author)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, MAX_CREDENTIAL_BYTES)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_have_one_version_and_explicit_empty_authority() {
        let current = Extension::new(Extended::default()).unwrap();
        assert_eq!(current.read().unwrap(), Extended::default());
        let body: Body = decode_bounded(&current.0, MAX_EXTENSION).unwrap();
        assert_eq!(body.version, EXTENSION_VERSION);
        for version in [0, 1, 3, u32::MAX] {
            let other = Extension(
                encode(&Body {
                    version,
                    authors: None,
                    device: None,
                    partitions: Vec::new(),
                })
                .unwrap(),
            );
            assert_eq!(
                other.read().unwrap_err().to_string(),
                "unsupported credential extension version"
            );
        }
        #[derive(Serialize)]
        struct MissingSelectors {
            version: u32,
        }
        let missing = Extension(
            encode(&MissingSelectors {
                version: EXTENSION_VERSION,
            })
            .unwrap(),
        );
        assert!(missing.read().is_err());
    }
}
