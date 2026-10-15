//! The enrollment and renewal messages, free of any transport or storage:
//! bytes in, bytes out. Enrollment is optional plumbing over issuance; a
//! device may equally install credentials supplied another way.
use crate::{
    Authority, Capability, CredentialChain, Error, IdentityProvider, MAX_CREDENTIAL_BYTES,
    MAX_ENDORSEMENT_BYTES, MAX_REVOCATION_SET_BYTES, PrivateKey, Profile, PublicKey, Purpose,
    RevocationSet, SignedCapability, decode_bounded, denied, encode, signing_bytes,
};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, Payload},
};
use curve25519_dalek::montgomery::MontgomeryPoint;
use p2panda_core::Signature;
use rand::RngCore;
use rocky_core::{Commitment, commitment};
use serde::{Deserialize, Serialize};

/// Wrong tokens an invitation tolerates before it locks.
const MAX_ATTEMPTS: u8 = 10;

fn equal(left: &[u8], right: &[u8]) -> bool {
    left.len() == right.len() && left.iter().zip(right).fold(0, |all, (a, b)| all | (a ^ b)) == 0
}
/// Bytes `seal` adds to its plaintext: the nonce and the AEAD tag.
pub(crate) const SEAL_OVERHEAD: usize = 12 + 16;
pub(crate) fn seal(key: Commitment, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let mut nonce = [0; 12];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    let sealed = ChaCha20Poly1305::new(&key.into())
        .encrypt(
            &nonce.into(),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| denied("sealing failed"))?;
    Ok([nonce.as_slice(), &sealed].concat())
}
pub(crate) fn open(key: Commitment, aad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, Error> {
    let (nonce, sealed) = sealed
        .split_at_checked(12)
        .ok_or_else(|| denied("sealed message is truncated"))?;
    let nonce: [u8; 12] = nonce
        .try_into()
        .map_err(|_| denied("sealed message is truncated"))?;
    ChaCha20Poly1305::new(&key.into())
        .decrypt(&nonce.into(), Payload { msg: sealed, aad })
        .map_err(|_| denied("the message was not sealed for this device"))
}

/// An issuer's record of one invitation. The token itself is a secret in
/// the issuer's key provider and in the invitation it hands out.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Invitation {
    digest: Commitment,
    pub expires: u64,
    attempts: u8,
    /// The pairing nonce that consumed it; that device may retry.
    consumed: Option<String>,
    /// What the invited device is granted.
    pub profile: Profile,
}
/// How a verified pairing request relates to its invitation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Admission {
    /// The invitation's first use.
    First,
    /// The consuming device asks again after a lost answer.
    Retry,
}
impl Invitation {
    /// Whether this token can enroll a new device without changing the invitation.
    pub fn is_open(&self, token: &[u8; 32], time: u64) -> bool {
        self.consumed.is_none()
            && self.attempts < MAX_ATTEMPTS
            && time <= self.expires
            && commitment("rocky.credentials.invitation.v1", token) == self.digest
    }

    pub fn open(token: &[u8; 32], profile: Profile, expires: u64) -> Self {
        Self {
            digest: commitment("rocky.credentials.invitation.v1", token),
            expires,
            attempts: 0,
            consumed: None,
            profile,
        }
    }
    /// Checks a request against the invitation and the issuer's `token`.
    /// A wrong proof is counted and locks the invitation after ten.
    /// Nothing is consumed: call `claim` once the request is acceptable.
    pub fn admit(
        &mut self,
        token: &[u8; 32],
        request: &PairRequest,
        time: u64,
    ) -> Result<Admission, Error> {
        if request.version != PairRequest::VERSION || request.nonce.len() != 64 {
            return Err(denied("invalid pairing request"));
        }
        if self.expires < time || self.attempts >= MAX_ATTEMPTS {
            return Err(denied("invitation expired or locked"));
        }
        if commitment("rocky.credentials.invitation.v1", token) != self.digest
            || !equal(&request.expected(token), &request.proof)
        {
            self.attempts += 1;
            return Err(denied("invalid invitation"));
        }
        match &self.consumed {
            None => Ok(Admission::First),
            Some(nonce) if *nonce == request.nonce => Ok(Admission::Retry),
            Some(_) => Err(denied("invitation already used")),
        }
    }
    /// Binds the invitation to the device of `request`; no other device
    /// can use it afterwards.
    pub fn claim(&mut self, request: &PairRequest) {
        self.consumed = Some(request.nonce.clone());
    }
}

/// What a joining node sends: its node key, the identity the invitation
/// grants to, a retry nonce and proof that it holds the invitation token.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PairRequest {
    pub version: u32,
    /// 64 hexadecimal digits, kept by the device across retries.
    pub nonce: String,
    pub node: PublicKey,
    /// The identity the invitation grants to.
    pub user: PublicKey,
    proof: Commitment,
    /// What the device expects to join, such as its application schema.
    pub context: Commitment,
    /// The joining device's own `(transport, address)` routes, so an issuer
    /// that enrolls another issuer can reach it. Bound by the proof.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<(String, String)>,
}
impl PairRequest {
    const VERSION: u32 = 3;
    pub fn new(
        token: &[u8; 32],
        nonce: String,
        node: PublicKey,
        user: PublicKey,
        context: Commitment,
    ) -> Self {
        let mut request = Self {
            version: Self::VERSION,
            nonce,
            node,
            user,
            proof: [0; 32],
            context,
            routes: Vec::new(),
        };
        request.proof = request.expected(token);
        request
    }
    /// The same request announcing the device's own routes.
    pub fn with_routes(mut self, token: &[u8; 32], routes: Vec<(String, String)>) -> Self {
        self.routes = routes;
        self.proof = self.expected(token);
        self
    }
    fn expected(&self, token: &[u8; 32]) -> Commitment {
        let mut bound = [
            token.as_slice(),
            self.nonce.as_bytes(),
            self.node.as_bytes(),
            self.user.as_bytes(),
            &self.context,
        ]
        .concat();
        // A request without routes keeps the proof of earlier versions.
        for (transport, address) in &self.routes {
            for part in [transport, address] {
                bound.extend_from_slice(&(part.len() as u64).to_le_bytes());
                bound.extend_from_slice(part.as_bytes());
            }
        }
        commitment("rocky.credentials.pair-proof.v3", &bound)
    }
    fn aad(&self) -> Vec<u8> {
        [
            b"rocky.credentials.pairing.v3".as_slice(),
            self.nonce.as_bytes(),
            self.node.as_bytes(),
            self.user.as_bytes(),
        ]
        .concat()
    }
    fn key(&self, token: &[u8; 32]) -> Commitment {
        commitment(
            "rocky.credentials.pair-key.v3",
            &[token.as_slice(), self.nonce.as_bytes()].concat(),
        )
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, MAX_CREDENTIAL_BYTES)
    }
}
/// Seals an enrollment to the device of `request` under the invitation
/// token, which only that device and the issuer hold.
pub fn seal_enrollment(
    token: &[u8; 32],
    request: &PairRequest,
    enrollment: &[u8],
) -> Result<Vec<u8>, Error> {
    seal(request.key(token), &request.aad(), enrollment)
}
pub fn open_enrollment(
    token: &[u8; 32],
    request: &PairRequest,
    sealed: &[u8],
) -> Result<Vec<u8>, Error> {
    open(request.key(token), &request.aad(), sealed)
}

/// The secret half of a one-time X25519 exchange.
pub struct Exchange([u8; 32]);
impl Exchange {
    pub fn new() -> (Self, [u8; 32]) {
        let mut secret = [0; 32];
        rand::rngs::OsRng.fill_bytes(&mut secret);
        (
            Self(secret),
            MontgomeryPoint::mul_base_clamped(secret).to_bytes(),
        )
    }
    pub fn shared(&self, peer: &[u8; 32]) -> Result<[u8; 32], Error> {
        let shared = MontgomeryPoint(*peer).mul_clamped(self.0).to_bytes();
        if shared == [0; 32] {
            return Err(denied("invalid key exchange"));
        }
        Ok(shared)
    }
}

/// A device asks an issuer to renew its credentials. It presents the chain
/// it was issued, current or not, and a one-time key for the answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenewRequest {
    version: u32,
    /// The issuer's node key: a request is not replayable to another.
    audience: PublicKey,
    pub node: PublicKey,
    time: u64,
    pub chain: CredentialChain,
    exchange: [u8; 32],
    signature: Signature,
}
#[derive(Serialize)]
struct RenewBody<'a> {
    version: u32,
    audience: &'a PublicKey,
    node: &'a PublicKey,
    time: u64,
    chain: &'a CredentialChain,
    exchange: &'a [u8; 32],
}
impl RenewRequest {
    const VERSION: u32 = 2;
    fn body(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.renew.v2",
            &RenewBody {
                version: self.version,
                audience: &self.audience,
                node: &self.node,
                time: self.time,
                chain: &self.chain,
                exchange: &self.exchange,
            },
        )
    }
    pub fn sign(
        node: &PrivateKey,
        audience: PublicKey,
        chain: CredentialChain,
        time: u64,
    ) -> Result<(Self, Exchange), Error> {
        let (secret, exchange) = Exchange::new();
        let mut request = Self {
            version: Self::VERSION,
            audience,
            node: node.public_key(),
            time,
            chain,
            exchange,
            // Replaced below; a signature has no neutral value.
            signature: node.sign(&[]),
        };
        request.signature = node.sign(&request.body()?);
        Ok((request, secret))
    }
    /// Checks that the node signed this request for `audience` within
    /// `skew` seconds of `time`, and that the owner or an issuer once
    /// granted its identity. Returns the identity's capability to renew.
    /// A delegated chain is renewed by its delegator, never here.
    /// Revocation is checked by `Authority::renew`.
    pub fn verify(
        &self,
        audience: PublicKey,
        publisher: PublicKey,
        (time, skew): (u64, u64),
    ) -> Result<&Capability, Error> {
        if self.version != Self::VERSION
            || self.audience != audience
            || self.time.abs_diff(time) > skew
            || !self.node.verify(&self.body()?, &self.signature)
        {
            return Err(denied("invalid renewal request"));
        }
        let identity = self.chain.provenance(publisher, self.node)?;
        // The identity's link follows the publisher or an issuer link.
        let granted = match self.chain.0.len().checked_sub(3) {
            None => true,
            Some(parent) => self.chain.0[parent].capability.extended()?.issues,
        };
        if !granted {
            return Err(denied("a delegated credential is renewed by its delegator"));
        }
        Ok(identity)
    }
    fn id(&self) -> Result<Commitment, Error> {
        Ok(commitment(
            "rocky.credentials.renew-request.v2",
            &encode(self)?,
        ))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, MAX_CREDENTIAL_BYTES)
    }
}

/// An issuer's answer: a renewed identity capability with the issuer links
/// it follows, the revocations the issuer holds and a payload sealed to the
/// requesting node, such as its reader keys. The issuer's identity signs the
/// whole answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Renewal {
    pub capability: SignedCapability,
    /// The issuer links from the publisher to the answering identity;
    /// empty when the owner answers.
    pub issuers: CredentialChain,
    pub revocations: RevocationSet,
    exchange: [u8; 32],
    sealed: Vec<u8>,
    signature: Signature,
}
impl Renewal {
    fn key(
        request: &RenewRequest,
        exchange: &[u8; 32],
        shared: &[u8; 32],
    ) -> Result<Commitment, Error> {
        Ok(commitment(
            "rocky.credentials.renew-key.v4",
            &[
                shared.as_slice(),
                &request.exchange,
                exchange,
                &request.id()?,
            ]
            .concat(),
        ))
    }
    fn body(&self, request: &RenewRequest) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.renewal.v4",
            &(
                request.id()?,
                self.capability.id()?,
                commitment(
                    "rocky.credentials.renewal-issuers.v4",
                    &encode(&self.issuers)?,
                ),
                commitment(
                    "rocky.credentials.renewal-revocations.v4",
                    &encode(&self.revocations)?,
                ),
                self.exchange,
                commitment("rocky.credentials.renewal-payload.v4", &self.sealed),
            ),
        )
    }
    /// Renews the verified `request` and seals `payload` to its node. A
    /// node or identity `revocations` revokes is refused.
    pub fn seal(
        authority: &Authority<'_>,
        request: &RenewRequest,
        prior: &Capability,
        revocations: RevocationSet,
        validity: (u64, u64, u64),
        payload: &[u8],
    ) -> Result<Self, Error> {
        if revocations.revokes(&request.node) {
            return Err(denied("invalid or revoked credential"));
        }
        let capability = authority.renew(prior, &revocations, validity)?;
        Self::seal_capability(authority, request, capability, revocations, payload)
    }
    /// Seals epoch delivery under a currently verified chain without renewing it.
    pub fn seal_current(
        authority: &Authority<'_>,
        publisher: PublicKey,
        request: &RenewRequest,
        revocations: RevocationSet,
        time: u64,
        payload: &[u8],
    ) -> Result<Self, Error> {
        request
            .chain
            .verify_selectors(publisher, request.node, &revocations, time)?;
        let capability = request
            .chain
            .0
            .iter()
            .rev()
            .nth(1)
            .ok_or_else(|| denied("empty epoch request chain"))?
            .clone();
        Self::seal_capability(authority, request, capability, revocations, payload)
    }
    fn seal_capability(
        authority: &Authority<'_>,
        request: &RenewRequest,
        capability: SignedCapability,
        revocations: RevocationSet,
        payload: &[u8],
    ) -> Result<Self, Error> {
        let (secret, exchange) = Exchange::new();
        let shared = secret.shared(&request.exchange)?;
        let sealed = seal(
            Self::key(request, &exchange, &shared)?,
            b"rocky.credentials.renewal.v4",
            payload,
        )?;
        let mut renewal = Self {
            capability,
            issuers: authority.prefix().clone(),
            revocations,
            exchange,
            sealed,
            // Replaced below; a signature has no neutral value.
            signature: PrivateKey::new().sign(&[]),
        };
        renewal.signature = authority
            .identity
            .sign(Purpose::Statement, &renewal.body(request)?)?;
        Ok(renewal)
    }
    /// The identity that answered: the owner or an issuer of `publisher`.
    pub fn signer(&self, publisher: PublicKey) -> Result<PublicKey, Error> {
        if self.issuers.0.is_empty() {
            Ok(publisher)
        } else {
            self.issuers.issuer(publisher, None)
        }
    }
    /// Checks that the owner or an issuer of the pinned `publisher` answered
    /// exactly `request`, and opens the payload. The renewed chain is
    /// `chain`; its receiver verifies it under its revocations.
    pub fn open(
        &self,
        publisher: PublicKey,
        request: &RenewRequest,
        secret: &Exchange,
    ) -> Result<Vec<u8>, Error> {
        let signer = self
            .signer(publisher)
            .map_err(|_| denied("renewal is not signed by the domain's owner or an issuer"))?;
        if !signer.verify(&self.body(request)?, &self.signature) {
            return Err(denied(
                "renewal is not signed by the domain's owner or an issuer",
            ));
        }
        let shared = secret.shared(&self.exchange)?;
        open(
            Self::key(request, &self.exchange, &shared)?,
            b"rocky.credentials.renewal.v4",
            &self.sealed,
        )
    }
    /// The renewed identity's chain: the issuer links, then its capability.
    pub fn chain(&self) -> CredentialChain {
        let mut chain = self.issuers.clone();
        chain.0.push(self.capability.clone());
        chain
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(
            bytes,
            MAX_CREDENTIAL_BYTES + MAX_REVOCATION_SET_BYTES + MAX_ENDORSEMENT_BYTES,
        )
    }
}

/// A new node of an identity asks a node of the same identity for what the
/// identity holds, without any issuer or invitation: it proves the node
/// key and the identity, and brings a one-time key for the answer.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityRequest {
    version: u32,
    /// The answering node's key: a request is not replayable to another.
    audience: PublicKey,
    pub identity: PublicKey,
    pub node: PublicKey,
    time: u64,
    exchange: [u8; 32],
    node_signature: Signature,
    identity_signature: Signature,
}
#[derive(Serialize)]
struct IdentityBody<'a> {
    version: u32,
    audience: &'a PublicKey,
    identity: &'a PublicKey,
    node: &'a PublicKey,
    time: u64,
    exchange: &'a [u8; 32],
}
impl IdentityRequest {
    const VERSION: u32 = 1;
    fn body(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.identity-request.v1",
            &IdentityBody {
                version: self.version,
                audience: &self.audience,
                identity: &self.identity,
                node: &self.node,
                time: self.time,
                exchange: &self.exchange,
            },
        )
    }
    pub fn sign(
        identity: &dyn IdentityProvider,
        node: &PrivateKey,
        audience: PublicKey,
        time: u64,
    ) -> Result<(Self, Exchange), Error> {
        let (secret, exchange) = Exchange::new();
        let mut request = Self {
            version: Self::VERSION,
            audience,
            identity: identity.public_key()?,
            node: node.public_key(),
            time,
            exchange,
            // Replaced below; a signature has no neutral value.
            node_signature: node.sign(&[]),
            identity_signature: node.sign(&[]),
        };
        let body = request.body()?;
        request.node_signature = node.sign(&body);
        request.identity_signature = identity.sign(Purpose::Statement, &body)?;
        Ok((request, secret))
    }
    /// Checks that the node and `identity` both signed this request for
    /// `audience` within `skew` seconds of `time`.
    pub fn verify(
        &self,
        audience: PublicKey,
        identity: PublicKey,
        (time, skew): (u64, u64),
    ) -> Result<(), Error> {
        let body = self.body()?;
        if self.version != Self::VERSION
            || self.audience != audience
            || self.identity != identity
            || self.node == identity
            || self.time.abs_diff(time) > skew
            || !self.node.verify(&body, &self.node_signature)
            || !self.identity.verify(&body, &self.identity_signature)
        {
            return Err(denied("invalid identity request"));
        }
        Ok(())
    }
    fn id(&self) -> Result<Commitment, Error> {
        Ok(commitment(
            "rocky.credentials.identity-request.id.v1",
            &encode(self)?,
        ))
    }
    fn key(&self, exchange: &[u8; 32], shared: &[u8; 32]) -> Result<Commitment, Error> {
        Ok(commitment(
            "rocky.credentials.identity-key.v1",
            &[shared.as_slice(), &self.exchange, exchange, &self.id()?].concat(),
        ))
    }
    /// Seals `payload` to the requesting node alone.
    pub fn answer(&self, payload: &[u8]) -> Result<IdentityAnswer, Error> {
        let (secret, exchange) = Exchange::new();
        let shared = secret.shared(&self.exchange)?;
        Ok(IdentityAnswer {
            sealed: seal(
                self.key(&exchange, &shared)?,
                b"rocky.credentials.identity-answer.v1",
                payload,
            )?,
            exchange,
        })
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, 1024)
    }
}
/// What a node of the identity sealed to the requesting node.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityAnswer {
    exchange: [u8; 32],
    sealed: Vec<u8>,
}
impl IdentityAnswer {
    pub fn open(&self, request: &IdentityRequest, secret: &Exchange) -> Result<Vec<u8>, Error> {
        let shared = secret.shared(&self.exchange)?;
        open(
            request.key(&self.exchange, &shared)?,
            b"rocky.credentials.identity-answer.v1",
            &self.sealed,
        )
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, 3 * MAX_REVOCATION_SET_BYTES)
    }
}
