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
mod delivery;
mod discovery;
mod enroll;
mod extension;
mod families;
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
mod peer;
mod peer_rpc;
mod phrase_key;
mod possession;
mod revocation;
mod root;
mod signed;
mod tips;

pub use author_binding::AuthorBinding;
pub use checkpoint_record::{
    CHECKPOINT_RECORD_FORMAT, CheckpointRecordStatement, SignedCheckpointRecord,
};
pub use delivery::{DeliveryReceipt, MAX_DELIVERY_PAYLOAD, SealedDelivery};
pub use discovery::{DiscoverySecret, open_discovery, seal_discovery};
pub use enroll::{Exchange, IdentityAnswer, IdentityRequest, RenewRequest, Renewal};
pub use extension::{DatabaseContext, Extended, Extension, Origin};
pub use families::GrantFamilies;
pub use group::{GROUP_SECRET_LEN, GroupSecretEnvelope, GroupSecrets, derive_table_key};
pub use identity::{FileIdentityProvider, Identity, IdentityProvider, Publisher, Purpose};
pub use identity_key::IdentityKey;
pub use issuance::{Delegation, DelegationTerms, Validity};
pub use issue::{Authority, Grant, MAX_GRANTS, NEVER, Profile, Resource, device_chain};
pub use keys::{FileKeyProvider, KeyProvider, ProviderError, Vault, write_private};
pub use p2panda_core::{Hash, PrivateKey, PublicKey, Signature};
pub use partition::{PartitionContext, PartitionGrant};
pub use peer::PeerId;
pub use peer_rpc::{
    EpochRequest, IdentityEnrollment, IdentityEnrollmentRequest, IdentityReply,
    InvitedEnrollmentRequest, MAX_MESSAGE as MAX_PEER_RPC_MESSAGE, PeerAdmission, PeerError,
    PeerReply, PeerReplyFrame, PeerRequest, PeerRequestFrame, PeerResolutionProof,
    PeerResolutionRequest, PeerTrust, UnlinkReply, VERSION as PEER_RPC_VERSION,
};
pub use possession::NodePossession;
pub use revocation::{
    Cut, DAY, Head, Heads, LIFETIME_SKEW, MAX_CHAIN_LIFETIME, MAX_CUT_HEADS, MAX_CUT_LANES,
    MAX_LIVE_REVOCATIONS, MAX_REVOCATION_BYTES, MAX_REVOCATION_SET_BYTES, REVOCATION_SET_FULL,
    Revocation, RevocationMode, RevocationSet, SIGNATURE_OVERHEAD, SignedRevocation,
};
pub use root::RootKey;
pub use signed::{
    Attestation, Capability, Certificate, CredentialChain, Credentials, Endorsement,
    MAX_ENDORSEMENT_BYTES, SignedCapability, SignedManifest, Signer, Verified, VerifiedChain,
};

pub use tips::{
    AuthorMark, BOOKMARK_MOVE_FORMAT, BookmarkMoveStatement, ChainLink, Equivocation,
    MAX_BOOKMARK_NAME, OpensStage, PastCut, SEAL_FORMAT, SealAction, SealStatement,
    SignedBookmarkMove, SignedSeal, SignedTipOpening, TIP_OPENING_FORMAT, TipAuthority, TipGraph,
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
pub const EXTENSION_LABEL_PREFIX: &str = "rocky.ext.";
pub const MAX_EXTENSION_LABEL: usize = 128;
pub fn extension_message(label: &str, message: &[u8]) -> Result<Vec<u8>, Error> {
    let named = label
        .strip_prefix(EXTENSION_LABEL_PREFIX)
        .is_some_and(|name| !name.is_empty());
    if !named || label.len() > MAX_EXTENSION_LABEL || !label.is_ascii() {
        return Err(denied("an extension signs only under a rocky.ext. label"));
    }
    signing_bytes(label, &serde_bytes::Bytes::new(message))
}

#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod testing;

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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn an_extension_signs_only_under_its_own_label_space() {
        let message = extension_message("rocky.ext.counters.v1", b"claim").unwrap();
        assert_ne!(
            message,
            extension_message("rocky.ext.counters.v2", b"claim").unwrap()
        );
        for label in [
            "rocky.seal.v3",
            "rocky.ext.",
            "rocky.ext",
            "other.ext.x",
            "rocky.ext.é",
        ] {
            assert!(
                extension_message(label, b"claim").is_err(),
                "{label} is not an extension label"
            );
        }
        assert!(extension_message(&format!("rocky.ext.{}", "x".repeat(128)), b"").is_err());
    }
}
