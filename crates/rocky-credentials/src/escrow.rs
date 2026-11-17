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
const METADATA_VOUCH: &str = "rocky.metadata-escrow.vouch.v1";
const METADATA_VERSION: u32 = 1;
const MAX_VOUCHED_BYTES: usize = MAX_RECORD_BYTES + 64 * 1024;
const LENGTH_HIDING_BLOCK: usize = 256;
pub const MAX_ESCROW_PAYLOAD: usize = 16 * 1024;
const MAX_RECORD_BYTES: usize = MAX_ESCROW_PAYLOAD + 2 * LENGTH_HIDING_BLOCK;

pub fn escrow_tag(root: PublicKey, secret: &[u8; 32]) -> Commitment {
    commitment(TAG, &[root.as_bytes().as_slice(), secret].concat())
}

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
        let padded = payload.len().div_ceil(LENGTH_HIDING_BLOCK).max(1) * LENGTH_HIDING_BLOCK;
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
            || self.sealed.len() < crate::enroll::SEAL_OVERHEAD + 4 + LENGTH_HIDING_BLOCK
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CarriedEscrow {
    version: u32,
    scope: ResourceScope,
    epoch: KeyEpoch,
    signer: PublicKey,
    signed_at: u64,
    chain: Commitment,
    record: RootEscrow,
    signature: Signature,
}
impl CarriedEscrow {
    pub fn chain_digest(&self) -> Commitment {
        self.chain
    }
    pub fn complete(self, chain: CredentialChain) -> Result<VouchedEscrow, Error> {
        if commitment("rocky.credential-chain.v1", &encode(&chain)?) != self.chain {
            return Err(denied("escrow chain digest mismatch"));
        }
        Ok(VouchedEscrow {
            body: Vouch {
                version: self.version,
                scope: self.scope,
                epoch: self.epoch,
                signer: self.signer,
                signed_at: self.signed_at,
                chain,
                record: self.record,
            },
            signature: self.signature,
        })
    }
}
impl VouchedEscrow {
    pub fn carried(&self) -> Result<CarriedEscrow, Error> {
        Ok(CarriedEscrow {
            version: self.body.version,
            scope: self.body.scope,
            epoch: self.body.epoch,
            signer: self.body.signer,
            signed_at: self.body.signed_at,
            chain: commitment("rocky.credential-chain.v1", &encode(&self.body.chain)?),
            record: self.body.record.clone(),
            signature: self.signature,
        })
    }

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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MetadataEscrow {
    body: MetadataVouch,
    signature: Signature,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MetadataVouch {
    version: u32,
    database: crate::DatabaseContext,
    descriptor: Commitment,
    signer: PublicKey,
    signed_at: u64,
    chain: CredentialChain,
    record: RootEscrow,
}
impl MetadataEscrow {
    pub fn sign(
        node: &PrivateKey,
        chain: CredentialChain,
        (database, descriptor): (crate::DatabaseContext, Commitment),
        record: RootEscrow,
        signed_at: u64,
    ) -> Result<Self, Error> {
        let body = MetadataVouch {
            version: METADATA_VERSION,
            database,
            descriptor,
            signer: node.public_key(),
            signed_at,
            chain,
            record,
        };
        let signature = node.sign(&signing_bytes(METADATA_VOUCH, &body)?);
        Ok(Self { body, signature })
    }
    pub fn verify(
        &self,
        database: crate::DatabaseContext,
        descriptor: Commitment,
    ) -> Result<(), Error> {
        if self.body.version != METADATA_VERSION {
            return Err(denied("unsupported metadata escrow version"));
        }
        if self.body.database != database || self.body.descriptor != descriptor {
            return Err(denied("the metadata escrow belongs to another database"));
        }
        if !self
            .body
            .signer
            .verify(&signing_bytes(METADATA_VOUCH, &self.body)?, &self.signature)
        {
            return Err(denied("metadata escrow vouch signature mismatch"));
        }
        self.body.record.validate()
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::PrivateKey;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_chain_reference_reconstructs_the_exact_signed_persisted_vouch() -> Result<(), Error> {
        let node = PrivateKey::new();
        let root = RootKey::generate();
        let record = RootEscrow::seal(root.public_key(), [9; 32], &[7; 32])?;
        let scope = ResourceScope {
            domain: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
            encryption: [4; 32],
            table: [5; 32],
        };
        let original = VouchedEscrow::sign(
            &node,
            CredentialChain::default(),
            (scope, KeyEpoch::new(1, &[7; 32])),
            record,
            0,
        )?;
        let carried = original.carried()?;
        let mut conflicting = carried.clone();
        conflicting.chain = [0; 32];
        assert!(conflicting.complete(CredentialChain::default()).is_err());
        let complete = carried.complete(CredentialChain::default())?;
        assert_eq!(complete.to_bytes()?, original.to_bytes()?);
        complete.verify()?;
        Ok(())
    }

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
        assert!(
            !bytes.windows(5).any(|window| window == b"acme/"),
            "the record carries no payload in clear"
        );
        assert!(!bytes.windows(32).any(|window| window == secret));
        let longer = RootEscrow::seal(root.public_key(), tag, &[1; 200])?;
        assert_eq!(
            longer.sealed.len(),
            parsed.sealed.len(),
            "lengths are padded within a bucket"
        );
        assert!(
            parsed.open(&RootKey::generate()).is_err(),
            "another root opens nothing"
        );
        let device = PrivateKey::from_bytes(&[9; 32]);
        assert!(
            parsed
                .open(&RootKey::from_secret(*device.as_bytes()))
                .is_err()
        );
        let mut forged = parsed.clone();
        forged.tag = escrow_tag(root.public_key(), &[8; 32]);
        assert!(
            forged.open(&root).is_err(),
            "a changed tag fails authentication"
        );
        let mut flipped = parsed.clone();
        flipped.sealed[20] ^= 1;
        assert!(flipped.open(&root).is_err());
        assert_ne!(
            escrow_tag(RootKey::generate().public_key(), &secret),
            tag,
            "tags depend on the secret and the root"
        );
        Ok(())
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_metadata_escrow_binds_its_database_descriptor_and_signer() -> Result<(), Error> {
        let node = PrivateKey::new();
        let root = RootKey::generate();
        let record = RootEscrow::seal(root.public_key(), [9; 32], &[7; 32])?;
        let database = crate::DatabaseContext {
            publisher: root.public_key(),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
        };
        let vouched = MetadataEscrow::sign(
            &node,
            CredentialChain::default(),
            (database, [5; 32]),
            record,
            0,
        )?;
        vouched.verify(database, [5; 32])?;
        let decoded: MetadataEscrow = decode_bounded(&encode(&vouched)?, MAX_VOUCHED_BYTES)?;
        decoded.verify(database, [5; 32])?;
        assert!(
            vouched.verify(database, [6; 32]).is_err(),
            "another descriptor is refused"
        );
        let mut elsewhere = database;
        elsewhere.scope.domain = rocky_core::DomainId([4; 16]);
        assert!(vouched.verify(elsewhere, [5; 32]).is_err());
        let mut forged = vouched.clone();
        forged.body.signed_at = 1;
        assert!(forged.verify(database, [5; 32]).is_err());
        let mut other = vouched;
        other.body.version = METADATA_VERSION + 1;
        assert!(other.verify(database, [5; 32]).is_err());
        Ok(())
    }
}
