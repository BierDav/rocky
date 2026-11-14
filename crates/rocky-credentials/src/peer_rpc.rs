//! The single peer RPC contract. Routes and endpoint identities grant no trust.
use crate::{
    Error, IdentityRequest, PairRequest, PrivateKey, PublicKey, RenewRequest, RevocationSet,
    SealFinality, Signature, SignedCapability, SignedSeal, denied, signing_bytes,
};
use rocky_core::Commitment;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EpochRequest {
    pub request: RenewRequest,
    pub revocations: RevocationSet,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityEnrollmentRequest {
    pub request: IdentityRequest,
    pub code: Option<String>,
}

const RESOLUTION_VERSION: u32 = 1;
/// A fresh challenge for the node behind a pinned contact.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerResolutionRequest {
    version: u32,
    challenge: Commitment,
}
/// Proof of node-key possession, without exposing credentials.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerResolutionProof {
    version: u32,
    challenge: Commitment,
    node: PublicKey,
    signature: Signature,
}
impl PeerResolutionRequest {
    pub fn fresh() -> Self {
        let mut challenge = [0; 32];
        rocky_platform::entropy::fill(&mut challenge);
        Self {
            version: RESOLUTION_VERSION,
            challenge,
        }
    }
    pub fn answer(&self, local: &PrivateKey) -> Result<PeerResolutionProof, Error> {
        if self.version != RESOLUTION_VERSION {
            return Err(denied("unsupported peer resolution version"));
        }
        let node = local.public_key();
        let bytes = signing_bytes(
            "rocky.peer-resolution.v1",
            &(self.version, self.challenge, node),
        )?;
        Ok(PeerResolutionProof {
            version: self.version,
            challenge: self.challenge,
            node,
            signature: local.sign(&bytes),
        })
    }
}
impl PeerResolutionProof {
    pub fn verify(
        &self,
        request: &PeerResolutionRequest,
        expected: crate::PeerId,
    ) -> Result<PublicKey, Error> {
        let bytes = signing_bytes(
            "rocky.peer-resolution.v1",
            &(self.version, self.challenge, self.node),
        )?;
        if self.version != RESOLUTION_VERSION
            || request.version != RESOLUTION_VERSION
            || self.challenge != request.challenge
            || crate::PeerId::of(self.node) != expected
            || !self.node.verify(&bytes, &self.signature)
        {
            return Err(denied(
                "peer resolution does not prove the pinned reference",
            ));
        }
        Ok(self.node)
    }
}
/// Only verified finality establishes the outcome of a private candidate.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StagedCloseStatus {
    Pending,
    Finality(SealFinality),
    Conflict {
        seal: Box<SignedSeal>,
        finality: SealFinality,
    },
}

pub const VERSION: u32 = 1;
pub const MAX_MESSAGE: usize = 3 * crate::MAX_REVOCATION_SET_BYTES;

#[derive(Serialize, Deserialize)]
pub enum PeerRequest {
    Pair(PairRequest),
    Renew(RenewRequest),
    Epochs(EpochRequest),
    Policy,
    Identity(IdentityEnrollmentRequest),
    LinkEnroll(Box<SignedCapability>),
    Unlink(RevocationSet),
    Resolve(PeerResolutionRequest),
    TipSchema { manifest: Commitment },
    StagedClose(SignedSeal),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerTrust {
    Public,
    Invitation,
    Enrolled,
    Identity,
    Issuer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerAdmission {
    Enrollment,
    Membership,
    Metadata,
}
impl PeerRequest {
    /// The handler verifies this proof; the class selects its inbound budget.
    pub fn requirements(&self) -> (PeerTrust, PeerAdmission) {
        match self {
            Self::Pair(_) => (PeerTrust::Invitation, PeerAdmission::Enrollment),
            Self::Renew(_) | Self::Epochs(_) => (PeerTrust::Enrolled, PeerAdmission::Enrollment),
            Self::Identity(_) | Self::LinkEnroll(_) | Self::Unlink(_) => {
                (PeerTrust::Identity, PeerAdmission::Membership)
            }
            Self::Policy | Self::Resolve(_) | Self::TipSchema { .. } => {
                (PeerTrust::Public, PeerAdmission::Metadata)
            }
            Self::StagedClose(_) => (PeerTrust::Issuer, PeerAdmission::Metadata),
        }
    }
    pub fn accepts(&self, reply: &PeerReply) -> bool {
        matches!(
            (self, reply),
            (Self::Pair(_), PeerReply::Pair(_))
                | (Self::Renew(_), PeerReply::Renew(_))
                | (Self::Epochs(_), PeerReply::Epochs(_))
                | (Self::Policy, PeerReply::Policy(_))
                | (Self::Identity(_), PeerReply::Identity(_))
                | (Self::LinkEnroll(_), PeerReply::LinkEnroll(_))
                | (Self::Unlink(_), PeerReply::Unlink(_))
                | (Self::Resolve(_), PeerReply::Resolve(_))
                | (Self::TipSchema { .. }, PeerReply::TipSchema(_))
                | (Self::StagedClose(_), PeerReply::StagedClose(_))
        )
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum IdentityReply {
    ConfirmationRequired,
    Enrolled(#[serde(with = "serde_bytes")] Vec<u8>),
}
#[derive(Serialize, Deserialize)]
pub enum PeerReply {
    Pair(#[serde(with = "serde_bytes")] Vec<u8>),
    Renew(#[serde(with = "serde_bytes")] Vec<u8>),
    Epochs(#[serde(with = "serde_bytes")] Vec<u8>),
    Policy(RevocationSet),
    Identity(IdentityReply),
    LinkEnroll(RevocationSet),
    Unlink(RevocationSet),
    Resolve(PeerResolutionProof),
    TipSchema(Option<Vec<u8>>),
    StagedClose(StagedCloseStatus),
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}
impl std::fmt::Display for PeerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for PeerError {}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerRequestFrame {
    pub version: u32,
    pub request: PeerRequest,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerReplyFrame {
    pub version: u32,
    pub result: Result<PeerReply, PeerError>,
}

impl PeerRequestFrame {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let frame: Self = decode(bytes)?;
        if frame.version != VERSION {
            return Err(denied("unsupported peer RPC version"));
        }
        Ok(frame)
    }
}
impl PeerReplyFrame {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let frame: Self = decode(bytes)?;
        if frame.version != VERSION {
            return Err(denied("unsupported peer RPC version"));
        }
        Ok(frame)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        if self.version != VERSION {
            return Err(denied("unsupported peer RPC version"));
        }
        let bytes = encode(self)?;
        if bytes.len() > MAX_MESSAGE {
            return Err(denied("peer RPC quota"));
        }
        Ok(bytes)
    }
}
impl PeerRequest {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        #[derive(Serialize)]
        struct Frame<'a> {
            version: u32,
            request: &'a PeerRequest,
        }
        let bytes = encode(&Frame {
            version: VERSION,
            request: self,
        })?;
        if bytes.len() > MAX_MESSAGE {
            return Err(denied("peer RPC quota"));
        }
        Ok(bytes)
    }
}

fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    crate::encode(value)
}
fn decode<T: Serialize + serde::de::DeserializeOwned>(bytes: &[u8]) -> Result<T, Error> {
    crate::decode_bounded(bytes, MAX_MESSAGE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn peer_resolution_binds_the_challenge_and_pinned_reference() {
        let node = PrivateKey::new();
        let request = PeerResolutionRequest::fresh();
        let proof = request.answer(&node).unwrap();
        let expected = crate::PeerId::of(node.public_key());
        assert_eq!(proof.verify(&request, expected).unwrap(), node.public_key());
        assert!(
            proof
                .verify(&PeerResolutionRequest::fresh(), expected)
                .is_err()
        );
        assert!(
            proof
                .verify(&request, crate::PeerId::of(PrivateKey::new().public_key()))
                .is_err()
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn canonical_rpc_refuses_json_trailing_bytes_unknown_variants_and_wrong_replies() {
        let request = PeerRequest::Policy;
        let bytes = encode(&PeerRequestFrame {
            version: VERSION,
            request,
        })
        .unwrap();
        let frame: PeerRequestFrame = decode(&bytes).unwrap();
        assert_eq!(frame.version, VERSION);
        assert_eq!(
            frame.request.requirements(),
            (PeerTrust::Public, PeerAdmission::Metadata)
        );
        let mut trailing = bytes;
        trailing.push(0);
        assert!(decode::<PeerRequestFrame>(&trailing).is_err());
        assert!(decode::<PeerRequestFrame>(br#"{"reply":"00"}"#).is_err());
        #[derive(Serialize)]
        enum Unknown {
            Future,
        }
        assert!(decode::<PeerRequest>(&encode(&Unknown::Future).unwrap()).is_err());
        assert!(!frame.request.accepts(&PeerReply::TipSchema(None)));
        assert!(
            frame
                .request
                .accepts(&PeerReply::Policy(RevocationSet::default()))
        );
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn other_rpc_versions_are_refused() {
        let bytes = encode(&PeerRequestFrame {
            version: VERSION + 1,
            request: PeerRequest::Policy,
        })
        .unwrap();
        assert!(PeerRequestFrame::from_bytes(&bytes).is_err());
        let frame = PeerReplyFrame {
            version: VERSION + 1,
            result: Ok(PeerReply::TipSchema(None)),
        };
        assert!(PeerReplyFrame::from_bytes(&encode(&frame).unwrap()).is_err());
        assert!(frame.to_bytes().is_err());
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn refusal_preserves_code_and_retryability_in_binary() {
        for retryable in [false, true] {
            let bytes = encode(&PeerReplyFrame {
                version: VERSION,
                result: Err(PeerError {
                    code: "denied".into(),
                    message: "peer refused".into(),
                    retryable,
                }),
            })
            .unwrap();
            let frame: PeerReplyFrame = decode(&bytes).unwrap();
            let Err(error) = frame.result else {
                panic!("refusal expected")
            };
            assert_eq!(error.code, "denied");
            assert_eq!(error.message, "peer refused");
            assert_eq!(error.retryable, retryable);
        }
    }
}
