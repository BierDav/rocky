use crate::{
    Attestation, Authority, Capability, CredentialChain, Error, IdentityProvider,
    MAX_CREDENTIAL_BYTES, MAX_ENDORSEMENT_BYTES, MAX_REVOCATION_SET_BYTES, PrivateKey, PublicKey,
    Purpose, RevocationSet, SignedCapability, decode_bounded, denied, encode, signing_bytes,
};
use chacha20poly1305::{
    ChaCha20Poly1305, KeyInit,
    aead::{Aead, Payload},
};
use curve25519_dalek::montgomery::MontgomeryPoint;
use p2panda_core::Signature;
use rocky_core::{Commitment, commitment};
use serde::{Deserialize, Serialize};

const NONCE_BYTES: usize = 12;
const AEAD_TAG_BYTES: usize = 16;
pub(crate) const SEAL_OVERHEAD: usize = NONCE_BYTES + AEAD_TAG_BYTES;
pub(crate) fn seal(key: Commitment, aad: &[u8], plaintext: &[u8]) -> Result<Vec<u8>, Error> {
    let nonce: [u8; NONCE_BYTES] = rocky_platform::entropy::random();
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
        .split_at_checked(NONCE_BYTES)
        .ok_or_else(|| denied("sealed message is truncated"))?;
    let nonce: [u8; NONCE_BYTES] = nonce
        .try_into()
        .map_err(|_| denied("sealed message is truncated"))?;
    ChaCha20Poly1305::new(&key.into())
        .decrypt(&nonce.into(), Payload { msg: sealed, aad })
        .map_err(|_| denied("the message was not sealed for this device"))
}

pub struct Exchange([u8; 32]);
impl Exchange {
    pub fn new() -> (Self, [u8; 32]) {
        let secret: [u8; 32] = rocky_platform::entropy::random();
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RenewRequest {
    version: u32,
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
            signature: placeholder_signature(node),
        };
        request.signature = node.sign(&request.body()?);
        Ok((request, secret))
    }
    pub fn verify(
        &self,
        audience: PublicKey,
        publisher: PublicKey,
        (time, skew): (u64, u64),
    ) -> Result<&Capability, Error> {
        self.verify_with_families(
            audience,
            publisher,
            (time, skew),
            &crate::GrantFamilies::default(),
        )
    }
    pub fn verify_with_families(
        &self,
        audience: PublicKey,
        publisher: PublicKey,
        (time, skew): (u64, u64),
        families: &crate::GrantFamilies,
    ) -> Result<&Capability, Error> {
        if self.version != Self::VERSION
            || self.audience != audience
            || self.time.abs_diff(time) > skew
            || !self.node.verify(&self.body()?, &self.signature)
        {
            return Err(denied("invalid renewal request"));
        }
        let identity = self
            .chain
            .historic_provenance_with_families(publisher, self.node, families)?;
        let capabilities = self.chain.capabilities();
        let follows_publisher_or_issuer = match capabilities.0.len().checked_sub(2) {
            None => true,
            Some(parent) => capabilities.0[parent].capability.extended()?.issues,
        };
        if !follows_publisher_or_issuer {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Renewal {
    pub capability: SignedCapability,
    pub issuers: CredentialChain,
    pub revocations: RevocationSet,
    exchange: [u8; 32],
    sealed: Vec<u8>,
    attestation: Attestation,
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
        Self::body_of(
            request,
            &self.capability,
            &self.issuers,
            &self.revocations,
            self.exchange,
            &self.sealed,
        )
    }
    fn body_of(
        request: &RenewRequest,
        capability: &SignedCapability,
        issuers: &CredentialChain,
        revocations: &RevocationSet,
        exchange: [u8; 32],
        sealed: &[u8],
    ) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.renewal.v5",
            &(
                request.id()?,
                capability.id()?,
                commitment("rocky.credentials.renewal-issuers.v5", &encode(issuers)?),
                commitment(
                    "rocky.credentials.renewal-revocations.v5",
                    &encode(revocations)?,
                ),
                exchange,
                commitment("rocky.credentials.renewal-payload.v5", sealed),
            ),
        )
    }
    pub fn seal(
        authority: &Authority<'_>,
        request: &RenewRequest,
        prior: &Capability,
        revocations: RevocationSet,
        validity: crate::Validity,
        payload: &[u8],
    ) -> Result<Self, Error> {
        if revocations.revokes(&request.node) {
            return Err(denied("invalid or revoked credential"));
        }
        let capability = authority.renew(prior, &revocations, validity)?;
        Self::seal_capability(authority, request, capability, revocations, payload)
    }
    pub fn seal_current(
        authority: &Authority<'_>,
        publisher: PublicKey,
        request: &RenewRequest,
        revocations: RevocationSet,
        time: u64,
        payload: &[u8],
    ) -> Result<Self, Error> {
        Self::seal_current_with_families(
            authority,
            publisher,
            request,
            revocations,
            time,
            payload,
            &crate::GrantFamilies::default(),
        )
    }
    pub fn seal_current_with_families(
        authority: &Authority<'_>,
        publisher: PublicKey,
        request: &RenewRequest,
        revocations: RevocationSet,
        time: u64,
        payload: &[u8],
        families: &crate::GrantFamilies,
    ) -> Result<Self, Error> {
        request
            .chain
            .verified_with_families(publisher, &revocations, time, families)?
            .selectors(request.node, &revocations, time)?;
        let capability = request
            .chain
            .capabilities()
            .0
            .pop()
            .ok_or_else(|| denied("empty epoch request chain"))?;
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
        let body = Self::body_of(
            request,
            &capability,
            authority.prefix(),
            &revocations,
            exchange,
            &sealed,
        )?;
        Ok(Self {
            capability,
            issuers: authority.prefix().clone(),
            revocations,
            exchange,
            sealed,
            attestation: authority.signer.attest(Purpose::Statement, &body)?,
        })
    }
    pub fn signer(&self, publisher: PublicKey) -> Result<PublicKey, Error> {
        if self.issuers.0.is_empty() {
            Ok(publisher)
        } else {
            self.issuers.issuer(publisher, None)
        }
    }
    pub fn open(
        &self,
        publisher: PublicKey,
        request: &RenewRequest,
        secret: &Exchange,
    ) -> Result<Vec<u8>, Error> {
        let signer = self
            .signer(publisher)
            .map_err(|_| denied("renewal is not signed by the domain's owner or an issuer"))?;
        if self.issuers.0.is_empty() != self.attestation.certificate().is_none()
            || self
                .attestation
                .verify(signer, &self.body(request)?)
                .is_err()
        {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityRequest {
    version: u32,
    audience: PublicKey,
    database: crate::DatabaseContext,
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
    database: crate::DatabaseContext,
    identity: &'a PublicKey,
    node: &'a PublicKey,
    time: u64,
    exchange: &'a [u8; 32],
}
impl IdentityRequest {
    const VERSION: u32 = 2;
    fn body(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.identity-request.v2",
            &IdentityBody {
                version: self.version,
                audience: &self.audience,
                database: self.database,
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
        database: crate::DatabaseContext,
    ) -> Result<(Self, Exchange), Error> {
        let (secret, exchange) = Exchange::new();
        let mut request = Self {
            version: Self::VERSION,
            audience,
            database,
            identity: identity.public_key()?,
            node: node.public_key(),
            time,
            exchange,
            node_signature: placeholder_signature(node),
            identity_signature: placeholder_signature(node),
        };
        let body = request.body()?;
        request.node_signature = node.sign(&body);
        request.identity_signature = identity.sign(Purpose::Statement, &body)?;
        Ok((request, secret))
    }
    pub fn verify(
        &self,
        audience: PublicKey,
        identity: PublicKey,
        (time, skew): (u64, u64),
        database: crate::DatabaseContext,
    ) -> Result<(), Error> {
        let body = self.body()?;
        if self.version != Self::VERSION
            || self.database != database
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
            "rocky.credentials.identity-request.id.v2",
            &encode(self)?,
        ))
    }
    fn key(&self, exchange: &[u8; 32], shared: &[u8; 32]) -> Result<Commitment, Error> {
        Ok(commitment(
            "rocky.credentials.identity-key.v2",
            &[shared.as_slice(), &self.exchange, exchange, &self.id()?].concat(),
        ))
    }
    pub fn confirmation_code(&self) -> Result<String, Error> {
        let digest = self.id()?;
        let number = u32::from_be_bytes(
            digest[..4]
                .try_into()
                .map_err(|_| denied("invalid request digest"))?,
        );
        Ok(format!("{:06}", number % 1_000_000))
    }
    pub fn answer(&self, audience: &PrivateKey, payload: &[u8]) -> Result<IdentityAnswer, Error> {
        if audience.public_key() != self.audience {
            return Err(denied("only the node a request names answers it"));
        }
        let (secret, exchange) = Exchange::new();
        let shared = secret.shared(&self.exchange)?;
        let sealed = seal(
            self.key(&exchange, &shared)?,
            b"rocky.credentials.identity-answer.v3",
            payload,
        )?;
        let signature = audience.sign(&IdentityAnswer::body(self, &exchange, &sealed)?);
        Ok(IdentityAnswer {
            exchange,
            sealed,
            signature,
        })
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        encode(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode_bounded(bytes, 1024)
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IdentityAnswer {
    exchange: [u8; 32],
    sealed: Vec<u8>,
    signature: Signature,
}
impl IdentityAnswer {
    fn body(
        request: &IdentityRequest,
        exchange: &[u8; 32],
        sealed: &[u8],
    ) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.credentials.identity-answer.v3",
            &(
                request.id()?,
                exchange,
                commitment("rocky.credentials.identity-answer-payload.v3", sealed),
            ),
        )
    }
    pub fn open(&self, request: &IdentityRequest, secret: &Exchange) -> Result<Vec<u8>, Error> {
        if !request.audience.verify(
            &Self::body(request, &self.exchange, &self.sealed)?,
            &self.signature,
        ) {
            return Err(denied(
                "the answer is not signed by the node its request names",
            ));
        }
        let shared = secret.shared(&self.exchange)?;
        open(
            request.key(&self.exchange, &shared)?,
            b"rocky.credentials.identity-answer.v3",
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

fn placeholder_signature(node: &PrivateKey) -> Signature {
    node.sign(&[])
}
