use crate::{
    Error, IdentityRequest, PersonalAnswer, PersonalAuthorization, PersonalChallenge,
    PersonalHello, PersonalPublication, PrivateKey, PublicKey, RenewRequest, RevocationSet,
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
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InvitedEnrollmentRequest {
    pub request: IdentityRequest,
    pub invitation: Commitment,
    binding: Signature,
}
impl InvitedEnrollmentRequest {
    pub fn sign(
        request: IdentityRequest,
        invitation: Commitment,
        node: &PrivateKey,
    ) -> Result<Self, Error> {
        if request.node != node.public_key() {
            return Err(denied(
                "the invitation binding needs the request's node key",
            ));
        }
        let binding = node.sign(&Self::bound(&request, invitation)?);
        Ok(Self {
            request,
            invitation,
            binding,
        })
    }
    pub fn verify_binding(&self) -> Result<(), Error> {
        if !self
            .request
            .node
            .verify(&Self::bound(&self.request, self.invitation)?, &self.binding)
        {
            return Err(denied("the request is not bound to this invitation"));
        }
        Ok(())
    }
    fn bound(request: &IdentityRequest, invitation: Commitment) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.invited-enrollment.v1",
            &(
                rocky_core::commitment(
                    "rocky.credentials.invited-enrollment-request.v1",
                    &request.to_bytes()?,
                ),
                invitation,
            ),
        )
    }
}
#[derive(Serialize, Deserialize)]
pub enum IdentityEnrollment {
    Shared(IdentityEnrollmentRequest),
    PersonalHello(PersonalHello),
    Personal(Box<PersonalAuthorization>),
    Invited(InvitedEnrollmentRequest),
}
#[derive(Serialize, Deserialize)]
pub enum LinkEnrollment {
    Shared(Box<SignedCapability>),
    Personal(Box<PersonalPublication>),
    PersonalCustody(Box<crate::PersonalCustodyRenewal>),
}

const RESOLUTION_VERSION: u32 = 1;
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerResolutionRequest {
    version: u32,
    challenge: Commitment,
}
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
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnlinkReply {
    database: crate::DatabaseContext,
    known: RevocationSet,
    node: PublicKey,
    signature: Signature,
}
impl UnlinkReply {
    pub fn sign(
        known: RevocationSet,
        database: crate::DatabaseContext,
        node: &PrivateKey,
    ) -> Result<Self, Error> {
        known.verify_in(database)?;
        let public = node.public_key();
        let bytes = receipt_bytes(database, &known, public)?;
        Ok(Self {
            database,
            known,
            node: public,
            signature: node.sign(&bytes),
        })
    }
    pub fn verify(
        &self,
        database: crate::DatabaseContext,
        peer: crate::PeerId,
    ) -> Result<&RevocationSet, Error> {
        if self.database != database || crate::PeerId::of(self.node) != peer {
            return Err(denied("unlink receipt belongs to another peer or database"));
        }
        self.known.verify_in(database)?;
        let bytes = receipt_bytes(database, &self.known, self.node)?;
        if !self.node.verify(&bytes, &self.signature) {
            return Err(denied("invalid unlink receipt signature"));
        }
        Ok(&self.known)
    }
}
fn receipt_bytes(
    database: crate::DatabaseContext,
    known: &RevocationSet,
    node: PublicKey,
) -> Result<Vec<u8>, Error> {
    signing_bytes(
        "rocky.unlink-receipt.v1",
        &(database, known.replicated_commitment()?, node),
    )
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum StagedCloseStatus {
    Pending,
    Finality(SealFinality),
    Conflict {
        seal: Box<SignedSeal>,
        finality: SealFinality,
    },
}

pub const VERSION: u32 = 7;
pub const MAX_MESSAGE: usize = 3 * crate::MAX_REVOCATION_SET_BYTES;

#[derive(Serialize, Deserialize)]
pub enum PeerRequest {
    Renew(RenewRequest),
    Epochs(EpochRequest),
    Policy,
    Identity(IdentityEnrollment),
    LinkEnroll(LinkEnrollment),
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
    Root,
    Issuer,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PeerAdmission {
    Enrollment,
    Membership,
    Metadata,
}
impl PeerRequest {
    pub fn requirements(&self) -> (PeerTrust, PeerAdmission) {
        match self {
            Self::Renew(_) | Self::Epochs(_) => (PeerTrust::Enrolled, PeerAdmission::Enrollment),
            Self::Identity(IdentityEnrollment::Invited(_)) => {
                (PeerTrust::Invitation, PeerAdmission::Enrollment)
            }
            Self::Identity(IdentityEnrollment::Personal(_))
            | Self::LinkEnroll(LinkEnrollment::Personal(_) | LinkEnrollment::PersonalCustody(_)) => {
                (PeerTrust::Root, PeerAdmission::Membership)
            }
            Self::Identity(IdentityEnrollment::Shared(_))
            | Self::LinkEnroll(LinkEnrollment::Shared(_))
            | Self::Unlink(_) => (PeerTrust::Identity, PeerAdmission::Membership),
            Self::Identity(IdentityEnrollment::PersonalHello(_))
            | Self::Policy
            | Self::Resolve(_)
            | Self::TipSchema { .. } => (PeerTrust::Public, PeerAdmission::Metadata),
            Self::StagedClose(_) => (PeerTrust::Issuer, PeerAdmission::Metadata),
        }
    }
    pub fn accepts(&self, reply: &PeerReply) -> bool {
        matches!(
            (self, reply),
            (Self::Renew(_), PeerReply::Renew(_))
                | (Self::Epochs(_), PeerReply::Epochs(_))
                | (Self::Policy, PeerReply::Policy(_))
                | (
                    Self::Identity(IdentityEnrollment::Shared(_)),
                    PeerReply::Identity(
                        IdentityReply::ConfirmationRequired | IdentityReply::Enrolled(_)
                    )
                )
                | (
                    Self::Identity(IdentityEnrollment::Invited(_)),
                    PeerReply::Identity(IdentityReply::Enrolled(_))
                )
                | (
                    Self::Identity(IdentityEnrollment::PersonalHello(_)),
                    PeerReply::Identity(IdentityReply::PersonalChallenge(_))
                )
                | (
                    Self::Identity(IdentityEnrollment::Personal(_)),
                    PeerReply::Identity(IdentityReply::Personal(_))
                )
                | (Self::LinkEnroll(_), PeerReply::LinkEnroll(_))
                | (Self::Unlink(_), PeerReply::Unlink(_))
                | (Self::Resolve(_), PeerReply::Resolve(_))
                | (Self::TipSchema { .. }, PeerReply::TipSchema(_))
                | (Self::StagedClose(_), PeerReply::StagedClose(_))
        )
    }
}

#[derive(Debug, Serialize, Deserialize)]
pub enum IdentityReply {
    ConfirmationRequired,
    Enrolled(#[serde(with = "serde_bytes")] Vec<u8>),
    PersonalChallenge(Box<PersonalChallenge>),
    Personal(Box<PersonalAnswer>),
}
#[derive(Serialize, Deserialize)]
pub enum PeerReply {
    Renew(#[serde(with = "serde_bytes")] Vec<u8>),
    Epochs(#[serde(with = "serde_bytes")] Vec<u8>),
    Policy(RevocationSet),
    Identity(IdentityReply),
    LinkEnroll(RevocationSet),
    Unlink(Box<UnlinkReply>),
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
    fn unlink_receipts_bind_policy_database_and_peer_even_after_revocation() {
        let root = PrivateKey::from_bytes(&[1; 32]);
        let node = PrivateKey::from_bytes(&[2; 32]);
        let peer = crate::PeerId::of(node.public_key());
        let database = crate::DatabaseContext {
            publisher: root.public_key(),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
        };
        let signed = crate::SignedRevocation::sign_in(
            crate::Revocation {
                app: database.scope.app,
                issuer: root.public_key(),
                time: 100,
                nodes: [peer].into(),
                identities: Default::default(),
                capabilities: Default::default(),
                cut: Vec::new(),
                mode: crate::RevocationMode::Lost,
                recovery: None,
            },
            &root,
            &crate::CredentialChain::default(),
            database,
        )
        .unwrap();
        let statement = signed.id().unwrap();
        let known = RevocationSet::try_from(vec![signed]).unwrap();
        let receipt = UnlinkReply::sign(known, database, &node).unwrap();
        assert!(
            receipt
                .verify(database, peer)
                .unwrap()
                .get(&statement)
                .is_some()
        );
        assert!(
            receipt
                .verify(database, crate::PeerId::of(root.public_key()))
                .is_err()
        );
        let mut foreign = database;
        foreign.scope.domain = rocky_core::DomainId([4; 16]);
        assert!(receipt.verify(foreign, peer).is_err());
        let mut tampered = receipt.clone();
        tampered.known = RevocationSet::default();
        assert!(tampered.verify(database, peer).is_err());
        let mut forged = receipt.clone();
        forged.node = root.public_key();
        assert!(
            forged
                .verify(database, crate::PeerId::of(root.public_key()))
                .is_err()
        );
        let frame = PeerReplyFrame {
            version: VERSION,
            result: Ok(PeerReply::Unlink(Box::new(receipt))),
        };
        let decoded = PeerReplyFrame::from_bytes(&frame.to_bytes().unwrap()).unwrap();
        let Ok(PeerReply::Unlink(receipt)) = decoded.result else {
            panic!("unlink receipt");
        };
        assert!(
            receipt
                .verify(database, peer)
                .unwrap()
                .get(&statement)
                .is_some()
        );
        let old = PeerReplyFrame {
            version: VERSION - 1,
            result: Ok(PeerReply::Policy(RevocationSet::default())),
        };
        assert!(PeerReplyFrame::from_bytes(&encode(&old).unwrap()).is_err());
        let old = PeerRequestFrame {
            version: VERSION - 1,
            request: PeerRequest::Policy,
        };
        assert!(PeerRequestFrame::from_bytes(&encode(&old).unwrap()).is_err());
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
    fn personal_branches_declare_their_own_trust_and_replies() {
        let hello =
            PeerRequest::Identity(IdentityEnrollment::PersonalHello(PersonalHello::fresh()));
        assert_eq!(
            hello.requirements(),
            (PeerTrust::Public, PeerAdmission::Metadata)
        );
        assert!(
            !hello.accepts(&PeerReply::Identity(IdentityReply::ConfirmationRequired)),
            "a hello never takes a sealed answer"
        );
        assert!(!hello.accepts(&PeerReply::Identity(IdentityReply::Enrolled(Vec::new()))));
        let root = crate::RootKey::from_secret([1; 32]);
        let owner = root.identity();
        let node = PrivateKey::new();
        let database = crate::DatabaseContext {
            publisher: root.public_key(),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
        };
        let (authorization, _) = PersonalAuthorization::sign(
            (&root, &owner),
            &node,
            (database, PrivateKey::new().public_key()),
            ([4; 32], [5; 32]),
            crate::PersonalAct::Bootstrap { page: 0 },
        )
        .unwrap();
        let bootstrap = PeerRequest::Identity(IdentityEnrollment::Personal(Box::new(
            authorization.clone(),
        )));
        assert_eq!(
            bootstrap.requirements(),
            (PeerTrust::Root, PeerAdmission::Membership)
        );
        let answer = authorization.answer(b"page").unwrap();
        assert!(
            bootstrap.accepts(&PeerReply::Identity(IdentityReply::Personal(Box::new(
                answer.clone()
            ))))
        );
        assert!(
            !hello.accepts(&PeerReply::Identity(IdentityReply::Personal(Box::new(
                answer
            ))))
        );
        assert_eq!(
            PeerRequest::Policy.requirements(),
            (PeerTrust::Public, PeerAdmission::Metadata),
            "personal bootstrap is no credential for the other paths"
        );
        assert_eq!(
            PeerRequest::Unlink(RevocationSet::default()).requirements(),
            (PeerTrust::Identity, PeerAdmission::Membership)
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn an_invited_enrollment_is_an_invitation_admission_answered_only_by_an_enrollment() {
        let identity = PrivateKey::from_bytes(&[5; 32]);
        let node = PrivateKey::from_bytes(&[6; 32]);
        let issuer = PrivateKey::from_bytes(&[7; 32]);
        let database = crate::DatabaseContext {
            publisher: issuer.public_key(),
            scope: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
        };
        let (signed, _) =
            IdentityRequest::sign(&identity, &node, issuer.public_key(), 1_000, database).unwrap();
        let mut substituted =
            InvitedEnrollmentRequest::sign(signed.clone(), [9; 32], &node).unwrap();
        substituted.invitation = [8; 32];
        assert!(
            substituted.verify_binding().is_err(),
            "a request bound to one invitation redeems no other"
        );
        assert!(
            InvitedEnrollmentRequest::sign(signed.clone(), [9; 32], &issuer).is_err(),
            "only the request's node binds it"
        );
        let bound = InvitedEnrollmentRequest::sign(signed, [9; 32], &node).unwrap();
        let request = PeerRequest::Identity(IdentityEnrollment::Invited(bound));
        assert_eq!(
            request.requirements(),
            (PeerTrust::Invitation, PeerAdmission::Enrollment)
        );
        assert!(request.accepts(&PeerReply::Identity(IdentityReply::Enrolled(Vec::new()))));
        assert!(!request.accepts(&PeerReply::Identity(IdentityReply::ConfirmationRequired)));
        let frame = PeerRequestFrame::from_bytes(&request.to_bytes().unwrap()).unwrap();
        let PeerRequest::Identity(IdentityEnrollment::Invited(carried)) = frame.request else {
            panic!("the invited enrollment changed kind on the wire");
        };
        assert_eq!(carried.invitation, [9; 32]);
        carried.verify_binding().unwrap();
        carried
            .request
            .verify(
                issuer.public_key(),
                identity.public_key(),
                (1_000, 0),
                database,
            )
            .unwrap();
    }
}
