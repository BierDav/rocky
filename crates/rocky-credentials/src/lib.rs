macro_rules! never_implements {
    ($type:ty: $($bound:tt)+) => {
        const _: fn() = || {
            trait AmbiguousWhenImplemented<Marker> {
                fn check() {}
            }
            impl<T: ?Sized> AmbiguousWhenImplemented<()> for T {}
            struct Implemented;
            impl<T: ?Sized + $($bound)+> AmbiguousWhenImplemented<Implemented> for T {}
            <$type as AmbiguousWhenImplemented<_>>::check();
        };
    };
}

mod author_binding;
mod checkpoint_record;
mod claims;
mod coordination;
mod coordinator;
pub mod counters;
pub use coordinator::{CoordinationObserver, Coordinator, CoordinatorConnector};
pub use settings::{
    Coordination, CoordinatorBundle, DATABASE_SETTINGS_FORMAT, DatabaseSettingsStatement,
    MAX_BUNDLE_URL, MAX_COORDINATOR_BUNDLE, SignedDatabaseSettings,
};
mod delivery;
mod discovery;
mod enroll;
mod escrow;
mod existence_base;
mod extension;
mod families;
mod finality;
mod group;
mod identity;
mod identity_key;
mod issuance;
mod issue;
mod keys;
mod membership;
mod migration;
pub use membership::{MAX_MEMBERSHIP_NODES, MembershipLink, MembershipSet};
pub use migration::{MAX_MIGRATION_DESCRIPTOR_BYTES, SignedMigrationPublication};
mod partition;
mod passkey;
mod peer;
mod peer_rpc;
mod personal;
mod phrase_key;
mod reset;
mod revocation;
mod root;
mod settings;
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
pub use escrow::{
    CarriedEscrow, MAX_ESCROW_PAYLOAD, MetadataEscrow, RootEscrow, VouchedEscrow, escrow_tag,
};
pub use existence_base::{
    EXISTENCE_BASE_FORMAT, ExistenceBase, ExistenceBaseStatement, ExistenceCut, ExistenceIdentity,
    ExistenceLog, MAX_EXISTENCE_BASE_LOGS, SignedExistenceBase,
};
pub use extension::{DatabaseContext, Extended, Extension, Origin};
pub use families::GrantFamilies;
pub use finality::{
    MAX_FINALITY_ACKS, MAX_FINALITY_BYTES, MAX_TERMINAL_ACK_BYTES, NamedFormat,
    SEAL_FINALITY_FORMAT, SealFinality, TERMINAL_ACK_FORMAT, TerminalAck, TerminalAckStatement,
    unsupported_finality,
};
pub use group::{GROUP_SECRET_LEN, GroupSecretEnvelope, GroupSecrets, derive_table_key};
pub use identity::{
    CoordinatorControl, ExternalLogin, FileIdentityProvider, Identity, IdentityProvider,
    LoginProvider, Publisher, Purpose, ReaderScope, SigningDevice, TransportEndpoint,
};
pub use identity_key::IdentityKey;
pub use issuance::{Delegation, DelegationTerms, Validity};
pub use issue::{Authority, Grant, MAX_GRANTS, NEVER, Profile, Resource, device_chain};
pub use keys::{FileKeyProvider, KeyProvider, ProviderError, Vault, write_private};
pub use p2panda_core::{Hash, PrivateKey, PublicKey, Signature};
pub use partition::{PartitionContext, PartitionGrant};
pub use passkey::{
    DiscoveryCapability, PersonalAccount, PersonalLocator, PrfInput, PrfUnsupported,
    VerifiedPasskeyPrf,
};
pub use peer::PeerId;
pub use peer_rpc::{
    EpochRequest, IdentityEnrollment, IdentityEnrollmentRequest, IdentityReply, LinkEnrollment,
    MAX_MESSAGE as MAX_PEER_RPC_MESSAGE, PeerAdmission, PeerError, PeerReply, PeerReplyFrame,
    PeerRequest, PeerRequestFrame, PeerResolutionProof, PeerResolutionRequest, PeerTrust,
    StagedCloseStatus, UnlinkReply, VERSION as PEER_RPC_VERSION,
};
pub use personal::{
    DatabaseKind, MAX_PERSONAL_DEVICES, PersonalAct, PersonalAnswer, PersonalAudience,
    PersonalAuthorization, PersonalBinding, PersonalChallenge, PersonalContext,
    PersonalCustodyRenewal, PersonalError, PersonalHello, PersonalLink, PersonalNamespace,
    PersonalPublication,
};
pub use reset::{
    FenceLog, FenceScope, MAX_FENCE_LOGS, MAX_RESET_BYTES, MAX_RESET_FOLLOWS, MAX_RESET_KEEP,
    Reset, ResetAdmission, ResetStanding, SignedResetAdmission,
};
pub use revocation::{
    Cut, DAY, Head, Heads, LIFETIME_SKEW, MAX_CHAIN_LIFETIME, MAX_CUT_HEADS, MAX_CUT_LANES,
    MAX_LIVE_REVOCATIONS, MAX_RESET_SET_BYTES, MAX_REVOCATION_BYTES, MAX_REVOCATION_SET_BYTES,
    REVOCATION_SET_FULL, Revocation, RevocationMode, RevocationSet, RootDecision, RootRecovery,
    SIGNATURE_OVERHEAD, SignedRevocation,
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

pub const MAX_CREDENTIAL_DEPTH: usize = 8;
pub const MAX_CREDENTIAL_BYTES: usize = 256 * 1024;

#[derive(Debug)]
pub enum Error {
    Denied(String),
    Protocol(rocky_core::ProtocolError),
    Provider(ProviderError),
    Personal(PersonalError),
    UnsupportedFormat {
        record: &'static str,
        format: u32,
        supported: u32,
    },
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Denied(message) => f.write_str(message),
            Self::Protocol(error) => write!(f, "{error}"),
            Self::Provider(error) => write!(f, "{error}"),
            Self::Personal(error) => write!(f, "{error}"),
            Self::UnsupportedFormat {
                record,
                format,
                supported,
            } => write!(
                f,
                "a {record} of format {format}, written by another release: this release reads only format {supported}"
            ),
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

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    p2panda_core::cbor::encode_cbor(value).map_err(|_| denied("invalid canonical encoding"))
}
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
pub fn signing_bytes<T: Serialize>(label: &str, value: &T) -> Result<Vec<u8>, Error> {
    encode(&(label, value))
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod testing {
    use crate::{
        Authority, Capability, CredentialChain, Extended, Extension, IdentityProvider, PrivateKey,
        SignedCapability, Signer,
    };

    pub fn node(
        identity: &dyn IdentityProvider,
        chain: &CredentialChain,
    ) -> (PrivateKey, CredentialChain) {
        let node = PrivateKey::new();
        let chain = chain
            .clone()
            .certify(identity, node.public_key())
            .expect("a node certificate");
        (node, chain)
    }
    pub fn authority(
        identity: &dyn IdentityProvider,
        chain: &CredentialChain,
    ) -> Authority<'static> {
        let (node, chain) = node(identity, chain);
        let node: &'static PrivateKey = Box::leak(Box::new(node));
        Authority::issuer(node, &chain).expect("an issuer node")
    }
    pub fn signer<'a>(identity: &'a dyn IdentityProvider, chain: &CredentialChain) -> Signer<'a> {
        if chain.0.is_empty() {
            return Signer::Key(identity);
        }
        let node: &'static PrivateKey = Box::leak(Box::new(PrivateKey::new()));
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
    pub fn lane_of(identity: &dyn IdentityProvider) -> crate::PublicKey {
        lane(identity).signing_node().expect("a node")
    }
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

    pub async fn claim_contract(coordinator: &dyn crate::Coordinator, scope: rocky_core::Scope) {
        use crate::counters::{Claim, Lookup, Name};
        let name = Name::new("rocky.claim-contract").expect("a counter name");
        let claim = |claim| coordinator.claim(scope, name.clone(), claim);
        let first = Claim::fresh().expect("a claim");
        let taken = claim(first).await.expect("a first value");
        assert_eq!(
            claim(first).await,
            Ok(taken),
            "a retried claim returns its value"
        );
        let mut last = taken;
        for _ in 0..2 {
            let next = claim(Claim::fresh().expect("a claim"))
                .await
                .expect("a later value");
            assert!(next > last, "a new claim takes a greater value");
            last = next;
        }
        assert_eq!(
            claim(first).await,
            Ok(taken),
            "a retry after later claims returns its value"
        );
        let expired = Claim {
            id: rocky_core::commitment("rocky.claim-contract.expired", &[]),
            issued: 0,
        };
        assert!(
            claim(expired).await.is_err(),
            "an expired claim takes no value"
        );
        let reading = coordinator
            .counter(scope, name.clone(), Some(expired))
            .await
            .expect("a reading");
        assert_eq!(reading.value, last, "an expired claim leaves the counter");
        assert!(
            !matches!(reading.claim, Some(Lookup::Claimed(_))),
            "an expired claim holds no value"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn bounded_decoding_refuses_oversized_noncanonical_and_trailing_input() {
        let value = (7u32, "rocky".to_owned());
        let bytes = encode(&value).unwrap();
        assert_eq!(
            decode_bounded::<(u32, String)>(&bytes, bytes.len()).unwrap(),
            value
        );
        assert!(
            decode_bounded::<(u32, String)>(&bytes, bytes.len() - 1).is_err(),
            "input over its limit is refused"
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert!(decode_bounded::<(u32, String)>(&trailing, 1024).is_err());
        assert_eq!(bytes[..2], [0x82, 0x07], "a two-item array, then 7 inline");
        let noncanonical = [&[0x82, 0x18, 0x07][..], &bytes[2..]].concat();
        assert!(
            decode_bounded::<(u32, String)>(&noncanonical, 1024).is_err(),
            "7 in a needlessly long encoding is refused"
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn signing_bytes_are_the_canonical_encoding_of_the_label_and_value() {
        let value = [9u8; 32];
        let signed = signing_bytes("rocky.example.v1", &value).unwrap();
        assert_eq!(signed, encode(&("rocky.example.v1", value)).unwrap());
        assert_ne!(
            signed,
            signing_bytes("rocky.example.v2", &value).unwrap(),
            "one value signs differently under every label"
        );
    }
}
