//! Personal bootstrap: a device holding only the passkey proves the root,
//! the owner identity and its new node key to any authorized sibling or
//! custodian, which answers with the database's signed metadata and opaque
//! root escrow. No step relies on the transport or on the topic; the
//! responder never signs a grant or returns a plaintext key.
use crate::{
    DatabaseContext, Error, Exchange, IdentityProvider, PersonalNamespace, PrivateKey, PublicKey,
    Purpose, Signature, SignedCapability, SignedManifest, denied, signing_bytes,
};
use rocky_core::{ApplicationManifest, Commitment};
use serde::{Deserialize, Serialize};

/// The one current bootstrap format; others are refused.
const VERSION: u32 = 1;
const CHALLENGE: &str = "rocky.personal.challenge.v1";
const AUTHORIZATION: &str = "rocky.personal.authorization.v1";
const ANSWER: &[u8] = b"rocky.personal.bootstrap-answer.v1";
const ANSWER_KEY: &str = "rocky.personal.bootstrap-key.v1";

/// A fresh device's untrusted first message: a challenge the responder
/// signs. It proves nothing and unlocks nothing private.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalHello {
    version: u32,
    challenge: [u8; 32],
}
impl PersonalHello {
    pub fn fresh() -> Self {
        Self {
            version: VERSION,
            challenge: rocky_platform::entropy::random(),
        }
    }
}

/// The responder's answer to a hello: the root-signed genesis it holds,
/// a nonce the authorization must bind and its node's proof over both.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalChallenge {
    version: u32,
    #[serde(with = "serde_bytes")]
    genesis: Vec<u8>,
    nonce: [u8; 32],
    pub responder: PublicKey,
    signature: Signature,
}
impl PersonalChallenge {
    fn body(
        database: DatabaseContext,
        hello: &PersonalHello,
        nonce: &[u8; 32],
        (descriptor, responder): (Commitment, PublicKey),
    ) -> Result<Vec<u8>, Error> {
        signing_bytes(
            CHALLENGE,
            &(
                VERSION,
                database,
                hello.challenge,
                nonce,
                descriptor,
                responder,
            ),
        )
    }
    /// `node` answers `hello` for `database` with `genesis` and a fresh
    /// `nonce` it keeps to judge the authorization.
    pub fn answer(
        hello: &PersonalHello,
        database: DatabaseContext,
        genesis: &SignedManifest,
        node: &PrivateKey,
        nonce: [u8; 32],
    ) -> Result<Self, Error> {
        if hello.version != VERSION {
            return Err(denied("unsupported personal bootstrap version"));
        }
        let responder = node.public_key();
        let body = Self::body(database, hello, &nonce, (genesis.descriptor()?, responder))?;
        Ok(Self {
            version: VERSION,
            genesis: genesis.to_bytes()?,
            nonce,
            responder,
            signature: node.sign(&body),
        })
    }
    /// The genesis this challenge carries, if it is the derived root's
    /// personal genesis of `owner` in `namespace` at `database`, and the
    /// responder signed it with the nonce over `hello`. The genesis is
    /// checked before anything is signed over its descriptor.
    pub fn verify(
        &self,
        hello: &PersonalHello,
        database: DatabaseContext,
        (owner, namespace): (PublicKey, &PersonalNamespace),
    ) -> Result<(SignedManifest, ApplicationManifest), Error> {
        if self.version != VERSION || hello.version != VERSION {
            return Err(denied("unsupported personal bootstrap version"));
        }
        let genesis = SignedManifest::from_bytes(&self.genesis)?;
        let manifest = genesis.verify_personal(database.publisher, owner, namespace)?;
        let domain = namespace.domain(owner)?;
        if manifest.app != database.scope.app
            || domain != database.scope.domain
            || namespace.mesh(owner)? != database.scope.mesh
        {
            return Err(crate::PersonalError::NamespaceMismatch.into());
        }
        let body = Self::body(
            database,
            hello,
            &self.nonce,
            (genesis.descriptor()?, self.responder),
        )?;
        if !self.responder.verify(&body, &self.signature) {
            return Err(denied(
                "the personal challenge does not prove its responder",
            ));
        }
        Ok((genesis, manifest))
    }
    pub fn nonce(&self) -> [u8; 32] {
        self.nonce
    }
}

/// What a personal authorization lets its responder do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PersonalAct {
    /// Release bootstrap page `page`, sealed to the requesting node.
    Bootstrap { page: u32 },
    /// Admit the owner certificate `certificate` of the requesting node,
    /// published under `publication`. Retries repeat both.
    Publish {
        certificate: Commitment,
        publication: [u8; 16],
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    version: u32,
    database: DatabaseContext,
    /// The responder's node key: a claim is not replayable to another.
    audience: PublicKey,
    owner: PublicKey,
    node: PublicKey,
    descriptor: Commitment,
    nonce: [u8; 32],
    exchange: [u8; 32],
    act: PersonalAct,
}

/// The responder-side facts an authorization must match: what its own
/// pinned genesis says.
#[derive(Clone, Copy, Debug)]
pub struct PersonalAudience {
    pub database: DatabaseContext,
    pub owner: PublicKey,
    pub descriptor: Commitment,
    /// This responder's node key.
    pub node: PublicKey,
}

/// A personal request's proof: the root, the owner identity and the new
/// node each signed one claim bound to the database, the responder, the
/// genesis descriptor, the responder's nonce and the act. It proves access
/// to root material, so the shared identity-only path is no bypass.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalAuthorization {
    claim: Claim,
    root: Signature,
    owner: Signature,
    possession: Signature,
}
impl PersonalAuthorization {
    fn body(claim: &Claim) -> Result<Vec<u8>, Error> {
        signing_bytes(AUTHORIZATION, claim)
    }
    /// The root, its owner identity and `node` authorize `act` at the
    /// responder `audience`, for the descriptor and nonce of its challenge.
    pub fn sign(
        (root, owner): (&dyn IdentityProvider, &dyn IdentityProvider),
        node: &PrivateKey,
        (database, audience): (DatabaseContext, PublicKey),
        (descriptor, nonce): (Commitment, [u8; 32]),
        act: PersonalAct,
    ) -> Result<(Self, Exchange), Error> {
        if root.public_key()? != database.publisher {
            return Err(crate::PersonalError::WrongPublisher.into());
        }
        let (secret, exchange) = Exchange::new();
        let claim = Claim {
            version: VERSION,
            database,
            audience,
            owner: owner.public_key()?,
            node: node.public_key(),
            descriptor,
            nonce,
            exchange,
            act,
        };
        let body = Self::body(&claim)?;
        Ok((
            Self {
                root: root.sign(Purpose::Statement, &body)?,
                owner: owner.sign(Purpose::Statement, &body)?,
                possession: node.sign(&body),
                claim,
            },
            secret,
        ))
    }
    /// Checks the three signatures against what `audience`'s own genesis
    /// pins. The nonce is the caller's to judge.
    pub fn verify(&self, audience: &PersonalAudience) -> Result<(), Error> {
        let claim = &self.claim;
        let body = Self::body(claim)?;
        if claim.version != VERSION {
            return Err(denied("unsupported personal bootstrap version"));
        }
        if claim.database != audience.database || claim.audience != audience.node {
            return Err(crate::PersonalError::WrongBinding.into());
        }
        if claim.owner != audience.owner {
            return Err(crate::PersonalError::WrongOwner.into());
        }
        if claim.descriptor != audience.descriptor {
            return Err(crate::PersonalError::WrongBinding.into());
        }
        if claim.node == claim.owner
            || claim.node == claim.database.publisher
            || !claim.database.publisher.verify(&body, &self.root)
            || !claim.owner.verify(&body, &self.owner)
        {
            return Err(denied("invalid personal authorization"));
        }
        if !claim.node.verify(&body, &self.possession) {
            return Err(crate::PersonalError::Possession.into());
        }
        Ok(())
    }
    pub fn node(&self) -> PublicKey {
        self.claim.node
    }
    pub fn nonce(&self) -> [u8; 32] {
        self.claim.nonce
    }
    pub fn act(&self) -> PersonalAct {
        self.claim.act
    }
    fn key(&self, exchange: &[u8; 32], shared: &[u8; 32]) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            ANSWER_KEY,
            &[
                shared.as_slice(),
                &self.claim.exchange,
                exchange,
                &crate::encode(&self.claim)?,
            ]
            .concat(),
        ))
    }
    /// Seals `payload` to the authorized node alone.
    pub fn answer(&self, payload: &[u8]) -> Result<PersonalAnswer, Error> {
        let (secret, exchange) = Exchange::new();
        let shared = secret.shared(&self.claim.exchange)?;
        Ok(PersonalAnswer {
            sealed: crate::enroll::seal(self.key(&exchange, &shared)?, ANSWER, payload)?,
            exchange,
        })
    }
}

/// A bootstrap page sealed to the node one authorization named.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalAnswer {
    exchange: [u8; 32],
    #[serde(with = "serde_bytes")]
    sealed: Vec<u8>,
}
impl PersonalAnswer {
    pub fn open(
        &self,
        authorization: &PersonalAuthorization,
        secret: &Exchange,
    ) -> Result<Vec<u8>, Error> {
        let shared = secret.shared(&self.exchange)?;
        crate::enroll::open(
            authorization.key(&self.exchange, &shared)?,
            ANSWER,
            &self.sealed,
        )
    }
}

/// The publication of one owner device: its certificate and the
/// authorization naming that certificate.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalPublication {
    pub authorization: PersonalAuthorization,
    pub certificate: SignedCapability,
}
impl PersonalPublication {
    /// The authorization's act names exactly this certificate of its node.
    pub fn check(&self) -> Result<[u8; 16], Error> {
        match self.authorization.act() {
            PersonalAct::Publish {
                certificate,
                publication,
            } if certificate == self.certificate.id()?
                && self.certificate.capability.subject == self.authorization.node() =>
            {
                Ok(publication)
            }
            _ => Err(crate::PersonalError::WrongBinding.into()),
        }
    }
}

#[cfg(test)]
mod tests;
