use super::*;
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use p2panda_core::{Body, Header, Operation, validate_operation};
use rand::{RngCore, rngs::OsRng};
use rocky_core::{Commitment, ResourceScope, RowChange, RowWrite};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PackageKind {
    Invocation,
    Rows,
    CommandResult,
    Job,
    Blob,
    Checkpoint,
    /// Rows read on behalf of a peer that does not hold the scope.
    Read,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Extensions {
    pub manifest: Commitment,
    pub scope: ResourceScope,
    pub generation: u64,
    pub credentials: CredentialChain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<u32>,
    #[serde(
        default = "legacy_row_format",
        skip_serializing_if = "is_legacy_row_format"
    )]
    pub row_format: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<rocky_core::PartitionTuple>,
}

fn legacy_row_format() -> u8 {
    1
}
fn is_legacy_row_format(version: &u8) -> bool {
    *version == 1
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedFrame {
    pub(crate) header: Vec<u8>,
    pub(crate) body: Vec<u8>,
}
impl EncryptedFrame {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_SECURE_FRAME {
            return Err(denied("secure frame quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let frame: Self = decode(bytes)?;
        frame.operation()?;
        Ok(frame)
    }
    pub fn id(&self) -> Result<Hash, Error> {
        Ok(self.operation()?.hash)
    }
    pub fn scope(&self) -> Result<ResourceScope, Error> {
        Ok(self.operation()?.header.extensions.scope)
    }
    pub(crate) fn operation(&self) -> Result<Operation<Extensions>, Error> {
        let header: Header<Extensions> = decode(&self.header)?;
        if !matches!(
            (header.extensions.row_format, header.extensions.author),
            (1, None) | (2..=5, Some(1..=0x7fff_ffff))
        ) {
            return Err(denied(
                "unsupported row identity frame format; upgrade the runtime",
            ));
        }
        let extension = &header.extensions;
        if matches!(extension.row_format, 3 | 5) != extension.partition.is_some()
            || extension.partition.as_ref().is_some_and(|tuple| {
                tuple
                    .scope(extension.scope.domain, extension.scope.table)
                    .ok()
                    != Some(extension.scope)
            })
        {
            return Err(denied("invalid authenticated partition scope"));
        }
        if header.previous.len() > 64
            || header.seq_num > i64::MAX as u64
            || header.extensions.generation == 0
        {
            return Err(denied("invalid operation metadata"));
        }
        let operation = Operation {
            hash: header.hash(),
            header,
            body: Some(Body::new(&self.body)),
        };
        validate_operation(&operation)
            .map_err(|_| denied("invalid operation signature or payload"))?;
        Ok(operation)
    }
}

pub(crate) fn encrypt(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let mut nonce = [0; 24];
    OsRng.fill_bytes(&mut nonce);
    let ciphertext = XChaCha20Poly1305::new(key.into())
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| denied("encryption failed"))?;
    let mut bytes = nonce.to_vec();
    bytes.extend(ciphertext);
    Ok(bytes)
}
pub(crate) fn decrypt(key: &[u8; 32], aad: &[u8], bytes: &[u8]) -> Result<Vec<u8>, Error> {
    let nonce = bytes
        .get(..24)
        .ok_or_else(|| denied("missing encryption nonce"))?;
    XChaCha20Poly1305::new(key.into())
        .decrypt(
            XNonce::from_slice(nonce),
            Payload {
                msg: &bytes[24..],
                aad,
            },
        )
        .map_err(|_| denied("missing/wrong key or corrupt ciphertext"))
}
pub(crate) fn author(
    key: &PrivateKey,
    extensions: Extensions,
    encryption_key: &[u8; 32],
    log: (u64, Option<Hash>, u64),
    previous: Vec<Hash>,
    plaintext: &[u8],
) -> Result<EncryptedFrame, Error> {
    let bytes = encrypt(encryption_key, &encode(&extensions)?, plaintext)?;
    let body = Body::new(&bytes);
    let mut header = Header {
        version: 1,
        public_key: key.public_key(),
        signature: None,
        payload_size: body.size(),
        payload_hash: Some(body.hash()),
        timestamp: log.2,
        seq_num: log.0,
        backlink: log.1,
        previous,
        extensions,
    };
    header.sign(key);
    let frame = EncryptedFrame {
        header: header.to_bytes(),
        body: bytes,
    };
    frame.to_bytes()?;
    Ok(frame)
}

/// A frame's place in a transaction bundle. Every member carries the same
/// `members`: entry `i` names member `i`'s scope and sequence, sealed under
/// that member's scope key, so a reader learns only the partitions it holds
/// keys for and how many members there are.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct Membership {
    pub transaction: Commitment,
    pub index: u16,
    pub members: Vec<Vec<u8>>,
}
#[derive(Serialize, Deserialize)]
struct BundleBody {
    version: u8,
    membership: Membership,
    writes: Vec<RowWrite>,
}
#[derive(Serialize, Deserialize)]
struct WitnessBody {
    version: u8,
    body: Vec<u8>,
    creations: Vec<(Commitment, Commitment)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    basis: Option<(u64, Commitment)>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    releases: Vec<Commitment>,
}
/// The decrypted body of a row frame.
#[derive(Clone)]
pub(crate) enum RowPayload {
    Witnessed {
        payload: Box<Self>,
        creations: Vec<(Commitment, Commitment)>,
        basis: Option<(u64, Commitment)>,
        releases: Vec<Commitment>,
    },
    /// A single-scope batch, the format from before bundles.
    Rows(Vec<RowChange>),
    Bundle {
        membership: Membership,
        writes: Vec<RowWrite>,
    },
}
impl RowPayload {
    pub(crate) fn decode(plaintext: &[u8]) -> Result<Self, Error> {
        if let Ok(body) = decode::<WitnessBody>(plaintext) {
            if body.version != 1
                || body.basis.is_some_and(|(sequence, _)| sequence == 0)
                || body.releases.len() > 64
                || body.releases.windows(2).any(|pair| pair[0] >= pair[1])
                || body.creations.len() > 32
                || body.creations.windows(2).any(|pair| pair[0].0 >= pair[1].0)
                || decode::<WitnessBody>(&body.body).is_ok()
            {
                return Err(denied("invalid creation witness envelope"));
            }
            return Ok(Self::Witnessed {
                payload: Box::new(Self::decode(&body.body)?),
                creations: body.creations,
                basis: body.basis,
                releases: body.releases,
            });
        }
        // A batch is a CBOR array and a bundle a map, so at most one parses.
        if let Ok(changes) = decode::<Vec<RowChange>>(plaintext) {
            return Ok(Self::Rows(changes));
        }
        let body: BundleBody = decode(plaintext)?;
        if ![1, 2].contains(&body.version) {
            return Err(denied("unsupported bundle version"));
        }
        Ok(Self::Bundle {
            membership: body.membership,
            writes: body.writes,
        })
    }
    pub(crate) fn bundle(
        membership: Membership,
        writes: Vec<RowWrite>,
        identity: bool,
    ) -> Result<Vec<u8>, Error> {
        encode(&BundleBody {
            version: if identity { 2 } else { 1 },
            membership,
            writes,
        })
    }
    pub(crate) fn witness(
        body: Vec<u8>,
        creations: Vec<(Commitment, Commitment)>,
        basis: Option<(u64, Commitment)>,
        releases: Vec<Commitment>,
    ) -> Result<Vec<u8>, Error> {
        encode(&WitnessBody {
            version: 1,
            body,
            creations,
            basis,
            releases,
        })
    }
    pub(crate) fn protection(&self) -> (Option<Commitment>, &[Commitment]) {
        match self {
            Self::Witnessed {
                basis, releases, ..
            } => (basis.map(|(_, hash)| hash), releases),
            _ => (None, &[]),
        }
    }
    pub(crate) fn basis(&self) -> Option<(u64, Commitment)> {
        match self {
            Self::Witnessed { basis, .. } => *basis,
            _ => None,
        }
    }
    pub(crate) fn creations(&self) -> &[(Commitment, Commitment)] {
        match self {
            Self::Witnessed { creations, .. } => creations,
            _ => &[],
        }
    }
    pub(crate) fn membership(&self) -> Option<&Membership> {
        match self {
            Self::Witnessed { payload, .. } => payload.membership(),
            Self::Rows(_) => None,
            Self::Bundle { membership, .. } => Some(membership),
        }
    }
    pub(crate) fn into_writes(self) -> Vec<RowWrite> {
        match self {
            Self::Witnessed { payload, .. } => payload.into_writes(),
            Self::Rows(changes) => changes.into_iter().map(RowWrite::changed).collect(),
            Self::Bundle { writes, .. } => writes,
        }
    }
}

/// Separately encrypted outputs. The aggregate commitment covers ciphertext and routing metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedPackage {
    pub scope: ResourceScope,
    pub generation: u64,
    pub kind: PackageKind,
    pub(crate) bytes: Vec<u8>,
}
impl EncryptedPackage {
    pub fn seal(
        scope: ResourceScope,
        generation: u64,
        kind: PackageKind,
        key: &[u8; 32],
        bytes: &[u8],
    ) -> Result<Self, Error> {
        if generation == 0 || bytes.len() > rocky_core::MAX_PAYLOAD_BYTES {
            return Err(denied("package quota/generation"));
        }
        let encrypted = encrypt(key, &encode(&(scope, generation, &kind))?, bytes)?;
        Ok(Self {
            scope,
            generation,
            kind,
            bytes: encrypted,
        })
    }
    pub(crate) fn open(&self, key: &[u8; 32]) -> Result<Vec<u8>, Error> {
        decrypt(
            key,
            &encode(&(self.scope, self.generation, &self.kind))?,
            &self.bytes,
        )
    }
    pub fn commitment(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.encrypted-package.v1",
            &encode(self)?,
        ))
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PackageSet {
    pub commitments: Vec<Commitment>,
}
impl PackageSet {
    pub fn bind(packages: &[EncryptedPackage]) -> Result<Self, Error> {
        if packages.is_empty() || packages.len() > 64 {
            return Err(denied("package set quota"));
        }
        Ok(Self {
            commitments: packages
                .iter()
                .map(EncryptedPackage::commitment)
                .collect::<Result<_, _>>()?,
        })
    }
    pub fn commitment(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.encrypted-package-set.v1",
            &encode(self)?,
        ))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode(bytes)
    }
    /// Verify a coordinator-bound package without needing private subpackages.
    pub async fn verify_confirmed_subset(
        package: &rocky_core::StrongPackage,
        receipt: &rocky_core::StrongReceipt,
        coordinator: &dyn rocky_core::Coordinator,
        subset: &[EncryptedPackage],
    ) -> Result<Self, Error> {
        if package.confirmation()? != receipt.confirmation
            || package.sequence != receipt.sequence
            || subset
                .iter()
                .any(|part| part.scope.domain != package.request.scope)
        {
            return Err(denied("result receipt/subpackage scope mismatch"));
        }
        coordinator.verify(receipt).await?;
        let binding = Self::from_bytes(&package.rows)?;
        binding.verify_subset(subset)?;
        Ok(binding)
    }
    pub fn verify_subset(&self, packages: &[EncryptedPackage]) -> Result<(), Error> {
        if self.commitments.is_empty() || self.commitments.len() > 64 {
            return Err(denied("package set quota"));
        }
        for package in packages {
            if !self.commitments.contains(&package.commitment()?) {
                return Err(denied("unbound result subpackage"));
            }
        }
        Ok(())
    }
}
