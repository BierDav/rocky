//! Credentials independent of a database or a network: signed manifests,
//! capabilities and revocations, the identities they bind and the provider
//! port that supplies them, key providers, issuance and the enrollment and
//! renewal messages. A runtime enforces
//! what these verify. See `docs/runtime/credentials.md`.
mod author_binding;
mod checkpoint_record;
mod claims;
mod coordination;
mod delivery;
mod discovery;
mod enroll;
mod escrow;
mod existence_base;
mod extension;
mod finality;
mod group;
mod identity;
mod identity_key;
mod issue;
mod keys;
mod membership;
mod migration;
pub use membership::{MAX_MEMBERSHIP_NODES, MembershipLink, MembershipSet};
pub use migration::{MAX_MIGRATION_DESCRIPTOR_BYTES, SignedMigrationPublication};
mod partition;
mod peer;
mod phrase_key;
mod revocation;
mod root;
mod signed;
mod tips;

pub use author_binding::AuthorBinding;
pub use checkpoint_record::{
    CHECKPOINT_RECORD_FORMAT, CheckpointRecordStatement, SignedCheckpointRecord,
};
pub use claims::{
    CLAIM_LIFETIME, CLAIM_SKEW, ClaimRight, MAX_CLAIM_COUNTER_NAME, MAX_CLAIM_COUNTERS,
};
pub use coordination::{
    CoordinationGrant, MAX_APPLICATION_NAME, SignedCoordinationGrant, SignedTenantAdmission,
    TenantAdmission,
};
pub use delivery::{DeliveryReceipt, MAX_DELIVERY_PAYLOAD, SealedDelivery};
pub use discovery::{DiscoverySecret, open_discovery, seal_discovery};
pub use enroll::{
    Admission, Exchange, IdentityAnswer, IdentityRequest, Invitation, PairRequest, RenewRequest,
    Renewal, open_enrollment, seal_enrollment,
};
pub use escrow::{CarriedEscrow, MAX_ESCROW_PAYLOAD, RootEscrow, VouchedEscrow, escrow_tag};
pub use existence_base::{
    EXISTENCE_BASE_FORMAT, ExistenceBase, ExistenceBaseStatement, ExistenceCut, ExistenceIdentity,
    ExistenceLog, MAX_EXISTENCE_BASE_LOGS, SignedExistenceBase,
};
pub use extension::{
    AuthorRange, DatabaseContext, Extended, Extension, MAX_AUTHOR, Origin, RECOVERY_FIRST,
    RECOVERY_SLOT, RECOVERY_SLOTS,
};
pub use finality::{
    MAX_FINALITY_ACKS, MAX_FINALITY_BYTES, MAX_TERMINAL_ACK_BYTES, SEAL_FINALITY_FORMAT,
    SealFinality, TERMINAL_ACK_FORMAT, TerminalAck, TerminalAckStatement,
};
pub use group::{GROUP_SECRET_LEN, GroupSecretEnvelope, derive_table_key};
pub use identity::{
    CoordinatorControl, ExternalLogin, FileIdentityProvider, Identity, IdentityProvider,
    LoginProvider, Publisher, Purpose, ReaderScope, SigningDevice, TransportEndpoint,
};
pub use identity_key::IdentityKey;
pub use issue::{Authority, Grant, MAX_GRANTS, NEVER, Profile, Resource, device_chain};
pub use keys::{FileKeyProvider, KeyProvider, ProviderError, Vault, write_private};
pub use p2panda_core::{Hash, PrivateKey, PublicKey, Signature};
pub use partition::PartitionGrant;
pub use peer::PeerId;
pub use revocation::{
    Cut, DAY, Head, Heads, LIFETIME_SKEW, MAX_CHAIN_LIFETIME, MAX_CUT_HEADS, MAX_CUT_LANES,
    MAX_LIVE_REVOCATIONS, MAX_REVOCATION_BYTES, MAX_REVOCATION_SET_BYTES, REVOCATION_SET_FULL,
    Revocation, RevocationSet, RootRecovery, SIGNATURE_OVERHEAD, SignedRevocation,
};
pub use root::RootKey;
pub use signed::{
    Attestation, Capability, Certificate, CredentialChain, Credentials, Endorsement,
    MAX_ENDORSEMENT_BYTES, SignedCapability, SignedManifest, Signer, Verified, VerifiedChain,
};

pub use tips::{
    AuthorMark, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, ChainLink, Equivocation,
    GENESIS_LINE_FORMAT, GenesisLineStatement, LogPoint, MAX_BOOKMARK_NAME, OpensStage, PastCut,
    Recovery, SEAL_FORMAT, SealAction, SealStatement, SignedBookmarkMove, SignedGenesisLine,
    SignedSeal, SignedTipOpening, StrippedFinality, TIP_OPENING_FORMAT, TipAuthority, TipGraph,
    TipOpeningStatement, genesis_tip,
};

use serde::{Serialize, de::DeserializeOwned};

/// Longest delegation chain: publisher, issuer identities, identity, node.
pub const MAX_CREDENTIAL_DEPTH: usize = 8;
/// Largest signed credential structure.
pub const MAX_CREDENTIAL_BYTES: usize = 256 * 1024;

/// Why credentials were refused. `Denied` is final for the same input.
#[derive(Debug)]
pub enum Error {
    Denied(String),
    Protocol(rocky_core::ProtocolError),
    /// The key provider failed or holds no such secret.
    Provider(ProviderError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied(message) => f.write_str(message),
            Self::Protocol(error) => write!(f, "{error}"),
            Self::Provider(error) => write!(f, "{error}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<rocky_core::ProtocolError> for Error {
    fn from(error: rocky_core::ProtocolError) -> Self {
        Self::Protocol(error)
    }
}
pub(crate) fn denied(message: &str) -> Error {
    Error::Denied(message.into())
}

/// The pinned canonical CBOR encoding every signature covers.
pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    p2panda_core::cbor::encode_cbor(value).map_err(|_| denied("invalid canonical encoding"))
}
/// Decodes at most `limit` bytes and refuses noncanonical or trailing input.
pub fn decode_bounded<T: Serialize + DeserializeOwned>(
    bytes: &[u8],
    limit: usize,
) -> Result<T, Error> {
    if bytes.len() > limit {
        return Err(denied("secure frame quota"));
    }
    let value: T =
        p2panda_core::cbor::decode_cbor(bytes).map_err(|_| denied("invalid secure frame"))?;
    if encode(&value)? != bytes {
        return Err(denied("non-canonical secure frame"));
    }
    Ok(value)
}
/// The bytes a domain-separated signature covers.
pub fn signing_bytes<T: Serialize>(label: &str, value: &T) -> Result<Vec<u8>, Error> {
    encode(&(label, value))
}

/// Test support for this and other crates' tests; not a supported API.
#[doc(hidden)]
pub mod testing {
    use crate::{
        AuthorRange, Authority, Capability, CredentialChain, Extended, Extension, IdentityProvider,
        PrivateKey, SignedCapability, Signer,
    };

    /// A fresh node of `identity`, certified under `chain` with the first
    /// number of its range, and its chain.
    pub fn node(
        identity: &dyn IdentityProvider,
        chain: &CredentialChain,
    ) -> (PrivateKey, CredentialChain) {
        let node = PrivateKey::new();
        let author = chain
            .authors()
            .expect("an author range")
            .map_or(1, |range| range.first);
        let chain = chain
            .clone()
            .certify(identity, node.public_key(), author)
            .expect("a node certificate");
        (node, chain)
    }
    /// The issuer authority of a fresh node of `identity` under `chain`.
    /// The node key is leaked, so the authority lives for the test.
    pub fn authority(
        identity: &dyn IdentityProvider,
        chain: &CredentialChain,
    ) -> Authority<'static> {
        let (node, chain) = node(identity, chain);
        let node: &'static PrivateKey = Box::leak(Box::new(node));
        Authority::issuer(node, &chain).expect("an issuer node")
    }
    /// Who signs for `identity` under its issuer links `chain`: its own key
    /// when the chain is empty (the root), otherwise a fresh node of it,
    /// whose key is leaked.
    pub fn signer<'a>(identity: &'a dyn IdentityProvider, chain: &CredentialChain) -> Signer<'a> {
        if chain.0.is_empty() {
            return Signer::Key(identity);
        }
        let node: &'static PrivateKey = Box::leak(Box::new(PrivateKey::new()));
        // A key the chain does not end at still certifies a node of its own,
        // for statements meant to be refused.
        let certificate = certificate(
            identity,
            node,
            chain.0.last().map_or(0, |last| last.capability.not_before),
            chain.database_context(chain.0[0].issuer).ok(),
        );
        Signer::node(node, &CredentialChain(vec![certificate])).expect("a signing node")
    }
    std::thread_local! {
        static LANES: std::cell::RefCell<std::collections::BTreeMap<crate::PublicKey, (&'static PrivateKey, SignedCapability)>> =
            const { std::cell::RefCell::new(std::collections::BTreeMap::new()) };
    }
    /// The one node of `identity` this thread signs tip statements with,
    /// certified from time zero; its key is the lane.
    pub fn lane(identity: &dyn IdentityProvider) -> Signer<'static> {
        let public = identity.public_key().expect("a key");
        let (key, certificate) = LANES.with(|lanes| {
            lanes
                .borrow_mut()
                .entry(public)
                .or_insert_with(|| {
                    let key: &'static PrivateKey = Box::leak(Box::new(PrivateKey::new()));
                    (key, certificate(identity, key, 0, None))
                })
                .clone()
        });
        Signer::node(key, &CredentialChain(vec![certificate])).expect("a lane")
    }
    /// The fixture lane, certified in the database the authority chain names.
    pub fn lane_for(identity: &dyn IdentityProvider, chain: &CredentialChain) -> Signer<'static> {
        let public = identity.public_key().expect("an identity");
        let database = chain
            .database_context(chain.0.first().expect("authority").issuer)
            .expect("a database");
        let _ = lane(identity);
        let (key, linked) = LANES.with(|lanes| {
            let mut lanes = lanes.borrow_mut();
            let held = lanes.get_mut(&public).expect("a lane");
            if held.1.capability.extended().expect("extension").database != Some(database) {
                held.1 = certificate(identity, held.0, 0, Some(database));
            }
            held.clone()
        });
        Signer::node(key, &CredentialChain(vec![linked])).expect("a lane")
    }
    /// The lane `lane` signs on for `identity`.
    pub fn lane_of(identity: &dyn IdentityProvider) -> crate::PublicKey {
        lane(identity).signing_node().expect("a node")
    }
    /// The lane of `identity` once `lane` made one, else the key itself.
    pub fn lane_key(identity: crate::PublicKey) -> crate::PublicKey {
        LANES.with(|lanes| {
            lanes
                .borrow()
                .get(&identity)
                .map_or(identity, |(key, _)| key.public_key())
        })
    }
    fn certificate(
        identity: &dyn IdentityProvider,
        node: &PrivateKey,
        not_before: u64,
        database: Option<crate::DatabaseContext>,
    ) -> SignedCapability {
        SignedCapability::sign(
            Capability {
                subject: node.public_key(),
                kind: rocky_core::PrincipalKind::Node,
                grants: Vec::new(),
                not_before,
                expires: crate::NEVER,
                delegations: 0,
                extension: Extension::new(Extended {
                    node: Some(crate::PeerId::of(node.public_key())),
                    authors: Some(AuthorRange::new(1, 1).expect("a range")),
                    sequence: Some(0),
                    database: Some(database.unwrap_or(crate::DatabaseContext {
                        publisher: identity.public_key().expect("a publisher"),
                        scope: rocky_core::Scope {
                            mesh: rocky_core::MeshId([1; 16]),
                            app: rocky_core::AppId([2; 16]),
                            domain: rocky_core::DomainId([3; 16]),
                        },
                    })),
                    ..Extended::default()
                })
                .expect("an extension"),
            },
            identity,
        )
        .expect("a node certificate")
    }
}
