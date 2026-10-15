use super::*;
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use p2panda_core::{Body, Header, Operation, validate_operation};
use rand::{RngCore, rngs::OsRng};
use rocky_core::{Commitment, ResourceScope, RowChange, RowWrite};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};

/// Frames decoded recently, by a digest of their bytes. A frame is decoded
/// several times as it is received and applied, and each decoding encodes
/// the header again for its canonical form, its hash and its signature;
/// the result depends on the bytes alone.
static DECODED: LazyLock<Mutex<HashMap<Hash, Operation<Extensions>>>> =
    LazyLock::new(Mutex::default);
const DECODED_FRAMES: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub enum PackageKind {
    Checkpoint,
    /// Rows read on behalf of a peer that does not hold the scope.
    Read,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Extensions {
    pub manifest: Commitment,
    pub scope: ResourceScope,
    pub generation: rocky_core::KeyEpoch,
    /// The digest of the author's credential chain; see `Chain`.
    pub chain: Commitment,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub author: Option<u32>,
    pub row_format: u8,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub partition: Option<rocky_core::PartitionTuple>,
}

/// The current row frame format: the header names its chain by digest.
pub(crate) const ROW_FORMAT: u8 = 8;
/// The refusal of a frame whose credential chain the receiver lacks.
pub(crate) const INCOMPLETE_FRAME: &str = "incomplete frame: its credential chain is unknown";

/// A credential chain with the digest a frame header names it by. The digest
/// shares the chain's bytes between frames, sessions and stored operations;
/// it carries no authority, and every use still verifies the chain. See
/// `docs/decisions/0053-chains-by-reference.md`.
#[derive(Clone, Debug)]
pub(crate) struct Chain {
    digest: Commitment,
    credentials: Arc<CredentialChain>,
}
impl Chain {
    pub(crate) fn new(credentials: CredentialChain) -> Result<Self, Error> {
        Ok(Self::of_bytes(&encode(&credentials)?, credentials))
    }
    pub(crate) fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        Ok(Self::of_bytes(bytes, decode(bytes)?))
    }
    fn of_bytes(bytes: &[u8], credentials: CredentialChain) -> Self {
        Self {
            digest: rocky_core::commitment("rocky.credential-chain.v1", bytes),
            credentials: Arc::new(credentials),
        }
    }
    pub(crate) fn digest(&self) -> Commitment {
        self.digest
    }
    pub(crate) fn credentials(&self) -> &CredentialChain {
        &self.credentials
    }
}
impl Serialize for Chain {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.credentials.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for Chain {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(CredentialChain::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

/// A signed header, its sealed body and, beside them, the author's
/// credential chain. The header signs the chain's digest only: a session
/// carries the chain once and sends its frames without it, and a replica
/// stores it once. A frame without its chain is incomplete and is refused.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedFrame {
    #[serde(with = "serde_bytes")]
    pub(crate) header: Vec<u8>,
    #[serde(with = "serde_bytes")]
    pub(crate) body: Vec<u8>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    chain: Option<Chain>,
}
impl EncryptedFrame {
    /// A frame without its chain, as storage and a session page hold it.
    pub(crate) fn bare(header: Vec<u8>, body: Vec<u8>) -> Self {
        Self {
            header,
            body,
            chain: None,
        }
    }
    pub(crate) fn with_chain(mut self, chain: Chain) -> Self {
        self.chain = Some(chain);
        self
    }
    /// The frame under another signed header, as a forger alters one.
    #[cfg(test)]
    pub(crate) fn with_header(&self, header: Vec<u8>) -> Self {
        Self {
            header,
            ..self.clone()
        }
    }
    /// Removes the chain, which a session sends once before the frame.
    pub(crate) fn take_chain(&mut self) -> Option<Chain> {
        self.chain.take()
    }
    /// The digest the signed header names its chain by.
    pub(crate) fn chain_digest(&self) -> Result<Commitment, Error> {
        Ok(self.operation()?.header.extensions.chain)
    }
    /// The author's chain, which must be the one the header names.
    pub(crate) fn chain(&self) -> Result<&Chain, Error> {
        match &self.chain {
            Some(chain) if chain.digest() == self.chain_digest()? => Ok(chain),
            _ => Err(denied(INCOMPLETE_FRAME)),
        }
    }
    /// Bytes a page or a retention bound counts for the frame: its header
    /// and body with their encoding, without the chain.
    pub fn weight(&self) -> usize {
        self.header
            .len()
            .saturating_add(self.body.len())
            .saturating_add(32)
    }
    /// The frame with its chain, for a transport outside a native session.
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        if self.weight() > MAX_SECURE_FRAME {
            return Err(denied("secure frame quota"));
        }
        let bytes = encode(self)?;
        if bytes.len() > 2 * MAX_SECURE_FRAME {
            return Err(denied("secure frame quota"));
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let frame: Self = decode_bounded(bytes, 2 * MAX_SECURE_FRAME)?;
        if frame.weight() > MAX_SECURE_FRAME {
            return Err(denied("secure frame quota"));
        }
        frame.chain()?;
        Ok(frame)
    }
    pub fn id(&self) -> Result<Hash, Error> {
        Ok(self.operation()?.hash)
    }
    pub fn scope(&self) -> Result<ResourceScope, Error> {
        Ok(self.operation()?.header.extensions.scope)
    }
    pub(crate) fn operation(&self) -> Result<Operation<Extensions>, Error> {
        let digest = Hash::new(
            [
                &(self.header.len() as u64).to_le_bytes()[..],
                &self.header,
                &self.body,
            ]
            .concat(),
        );
        let decoded = || DECODED.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(operation) = decoded().get(&digest) {
            return Ok(operation.clone());
        }
        let operation = self.decode_operation()?;
        let mut decoded = decoded();
        if decoded.len() >= DECODED_FRAMES {
            decoded.clear();
        }
        decoded.insert(digest, operation.clone());
        Ok(operation)
    }
    fn decode_operation(&self) -> Result<Operation<Extensions>, Error> {
        let header: Header<Extensions> = decode(&self.header)?;
        if header.extensions.row_format != ROW_FORMAT {
            return Err(denied("unsupported row frame format"));
        }
        let extension = &header.extensions;
        if extension
            .author
            .is_some_and(|author| !(1..=0x7fff_ffff).contains(&author))
            || extension.partition.as_ref().is_some_and(|tuple| {
                tuple
                    .scope(extension.scope.domain, extension.scope.table)
                    .ok()
                    != Some(extension.scope)
            })
        {
            return Err(denied("invalid authenticated partition scope or author"));
        }
        if header.previous.len() > 64
            || header.seq_num > i64::MAX as u64
            || !header.extensions.generation.valid()
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
    chain: Chain,
    encryption_key: &[u8; 32],
    log: (u64, Option<Hash>, u64),
    previous: Vec<Hash>,
    plaintext: &[u8],
) -> Result<EncryptedFrame, Error> {
    if extensions.chain != chain.digest() {
        return Err(denied(INCOMPLETE_FRAME));
    }
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
    let frame = EncryptedFrame::bare(header.to_bytes(), bytes).with_chain(chain);
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
    /// The coordinated log position of a coordinated write's bundle.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entry: Option<u64>,
}
#[derive(Serialize, Deserialize)]
struct BundleBody {
    version: u8,
    proofs: BTreeMap<Commitment, crate::replica::moves::CreationProof>,
    carried: BTreeMap<Commitment, crate::replica::moves::Carried>,
    membership: Option<Membership>,
    writes: Vec<RowWrite>,
}
#[derive(Serialize, Deserialize)]
struct WitnessBody {
    version: u8,
    #[serde(with = "serde_bytes")]
    body: Vec<u8>,
    creations: Vec<(Commitment, Commitment)>,
    basis: Option<(u64, Commitment)>,
    releases: Vec<Commitment>,
}
/// A current row payload, always protected by a creation witness envelope.
#[derive(Clone)]
pub(crate) struct RowPayload {
    proofs: BTreeMap<Commitment, crate::replica::moves::CreationProof>,
    carried: BTreeMap<Commitment, crate::replica::moves::Carried>,
    membership: Option<Membership>,
    writes: Vec<RowWrite>,
    creations: Vec<(Commitment, Commitment)>,
    basis: Option<(u64, Commitment)>,
    releases: Vec<Commitment>,
}
impl RowPayload {
    pub(crate) fn check_formats(plaintext: &[u8]) -> Result<(), Error> {
        #[derive(Deserialize)]
        struct Envelope {
            version: u8,
            #[serde(with = "serde_bytes")]
            body: Vec<u8>,
        }
        #[derive(Deserialize)]
        struct Version {
            version: u8,
        }
        let envelope: Envelope = p2panda_core::cbor::decode_cbor(plaintext)
            .map_err(|_| denied("unsupported creation witness format"))?;
        if envelope.version != 1 {
            return Err(denied("unsupported creation witness format"));
        }
        let body: Version = p2panda_core::cbor::decode_cbor(envelope.body.as_slice())
            .map_err(|_| denied("unsupported bundle version"))?;
        if body.version != 4 {
            return Err(denied("unsupported bundle version"));
        }
        Ok(())
    }
    pub(crate) fn decode(plaintext: &[u8]) -> Result<Self, Error> {
        Self::check_formats(plaintext)?;
        let envelope: WitnessBody = decode(plaintext)?;
        if envelope.version != 1 {
            return Err(denied("unsupported creation witness format"));
        }
        if envelope.basis.is_some_and(|(sequence, _)| sequence == 0)
            || envelope.releases.len() > 64
            || envelope.releases.windows(2).any(|pair| pair[0] >= pair[1])
            || envelope.creations.len() > 32
            || envelope
                .creations
                .windows(2)
                .any(|pair| pair[0].0 >= pair[1].0)
        {
            return Err(denied("invalid creation witness envelope"));
        }
        let body: BundleBody = decode(&envelope.body)?;
        if body.version != 4 {
            return Err(denied("unsupported bundle version"));
        }
        Ok(Self {
            proofs: body.proofs,
            carried: body.carried,
            membership: body.membership,
            writes: body.writes,
            creations: envelope.creations,
            basis: envelope.basis,
            releases: envelope.releases,
        })
    }
    pub(crate) fn rows(changes: &[RowChange]) -> Result<Vec<u8>, Error> {
        encode(&BundleBody {
            version: 4,
            proofs: BTreeMap::new(),
            carried: BTreeMap::new(),
            membership: None,
            writes: changes.iter().cloned().map(RowWrite::changed).collect(),
        })
    }
    pub(crate) fn bundle(membership: Membership, writes: Vec<RowWrite>) -> Result<Vec<u8>, Error> {
        encode(&BundleBody {
            version: 4,
            proofs: BTreeMap::new(),
            carried: BTreeMap::new(),
            membership: Some(membership),
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
    pub(crate) fn stamp_body(
        bytes: &[u8],
        proofs: BTreeMap<Commitment, crate::replica::moves::CreationProof>,
        carried: BTreeMap<Commitment, crate::replica::moves::Carried>,
    ) -> Result<Vec<u8>, Error> {
        let mut body: BundleBody = decode(bytes)?;
        body.proofs = proofs;
        body.carried = carried;
        encode(&body)
    }
    pub(crate) fn proof(&self, row: &Commitment) -> Option<&crate::replica::moves::CreationProof> {
        self.proofs.get(row)
    }
    pub(crate) fn carried(&self, row: &Commitment) -> Option<&crate::replica::moves::Carried> {
        self.carried.get(row)
    }
    pub(crate) fn protection(&self) -> (Option<Commitment>, &[Commitment]) {
        (self.basis.map(|(_, hash)| hash), &self.releases)
    }
    pub(crate) fn basis(&self) -> Option<(u64, Commitment)> {
        self.basis
    }
    pub(crate) fn creations(&self) -> &[(Commitment, Commitment)] {
        &self.creations
    }
    pub(crate) fn membership(&self) -> Option<&Membership> {
        self.membership.as_ref()
    }
    pub(crate) fn into_writes(self) -> Vec<RowWrite> {
        self.writes
    }
}

/// Separately encrypted outputs. The aggregate commitment covers ciphertext and routing metadata.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedPackage {
    pub scope: ResourceScope,
    pub generation: rocky_core::KeyEpoch,
    pub kind: PackageKind,
    pub(crate) bytes: Vec<u8>,
}
impl EncryptedPackage {
    pub fn seal(
        scope: ResourceScope,
        generation: rocky_core::KeyEpoch,
        kind: PackageKind,
        key: &[u8; 32],
        bytes: &[u8],
    ) -> Result<Self, Error> {
        if !generation.valid() || bytes.len() > rocky_core::MAX_PAYLOAD_BYTES {
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
            "rocky.encrypted-package.v2",
            &encode(self)?,
        ))
    }
}
