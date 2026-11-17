use crate::{
    DatabaseContext, Error, Exchange, IdentityProvider, PersonalNamespace, PrivateKey, PublicKey,
    Purpose, Signature, SignedCapability, SignedManifest, denied, signing_bytes,
};
use rocky_core::{ApplicationManifest, Commitment};
use serde::{Deserialize, Serialize};

const VERSION: u32 = 1;
const CHALLENGE: &str = "rocky.personal.challenge.v1";
const AUTHORIZATION: &str = "rocky.personal.authorization.v1";
const ANSWER: &[u8] = b"rocky.personal.bootstrap-answer.v1";
const ANSWER_KEY: &str = "rocky.personal.bootstrap-key.v1";

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

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", deny_unknown_fields)]
pub enum PersonalAct {
    Bootstrap {
        page: u32,
    },
    Publish {
        certificate: Commitment,
        publication: [u8; 16],
    },
    RenewCustody {
        capability: Commitment,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Claim {
    version: u32,
    database: DatabaseContext,
    audience: PublicKey,
    owner: PublicKey,
    node: PublicKey,
    descriptor: Commitment,
    nonce: [u8; 32],
    exchange: [u8; 32],
    act: PersonalAct,
}

#[derive(Clone, Copy, Debug)]
pub struct PersonalAudience {
    pub database: DatabaseContext,
    pub owner: PublicKey,
    pub descriptor: Commitment,
    pub node: PublicKey,
}

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
    pub fn answer(&self, payload: &[u8]) -> Result<PersonalAnswer, Error> {
        let (secret, exchange) = Exchange::new();
        let shared = secret.shared(&self.claim.exchange)?;
        Ok(PersonalAnswer {
            sealed: crate::enroll::seal(self.key(&exchange, &shared)?, ANSWER, payload)?,
            exchange,
        })
    }
}

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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalPublication {
    pub authorization: PersonalAuthorization,
    pub certificate: SignedCapability,
}
impl PersonalPublication {
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

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PersonalCustodyRenewal {
    pub authorization: PersonalAuthorization,
    pub chain: crate::CredentialChain,
}
impl PersonalCustodyRenewal {
    pub fn renewed(&self) -> Result<&SignedCapability, Error> {
        let renewed = self
            .chain
            .0
            .last()
            .ok_or_else(|| denied("an empty custody renewal"))?;
        match self.authorization.act() {
            PersonalAct::RenewCustody { capability } if capability == renewed.id()? => Ok(renewed),
            _ => Err(crate::PersonalError::WrongBinding.into()),
        }
    }
}

#[cfg(test)]
mod tests;
