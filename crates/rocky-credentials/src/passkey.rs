use crate::{Error, IdentityKey, PersonalNamespace, PublicKey, RootKey};
use rocky_core::{ApplicationManifest, DomainId, MeshId, Scope};
use zeroize::{Zeroize, Zeroizing};

const PRF_INPUT: &str = "rocky.personal.prf-input.v1";
const DISCOVERY: &[u8] = b"rocky.personal.discovery.v1";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrfInput([u8; 32]);
impl PrfInput {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl PersonalNamespace {
    pub fn prf_input(&self) -> Result<PrfInput, Error> {
        Ok(PrfInput(rocky_core::commitment(
            PRF_INPUT,
            &self.canonical()?,
        )))
    }
}

pub struct VerifiedPasskeyPrf {
    input: PrfInput,
    output: Zeroizing<[u8; 32]>,
}
never_implements!(VerifiedPasskeyPrf: Clone);
never_implements!(VerifiedPasskeyPrf: serde::Serialize);
impl VerifiedPasskeyPrf {
    pub fn new(input: &PrfInput, mut output: [u8; 32]) -> Self {
        let verified = Self {
            input: *input,
            output: Zeroizing::new(output),
        };
        output.zeroize();
        verified
    }
}
impl std::fmt::Debug for VerifiedPasskeyPrf {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("VerifiedPasskeyPrf(<redacted>)")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrfUnsupported;
impl std::fmt::Display for PrfUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the passkey's PRF output is unavailable for this namespace")
    }
}
impl std::error::Error for PrfUnsupported {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersonalLocator {
    pub publisher: PublicKey,
    pub owner: PublicKey,
    pub mesh: MeshId,
    pub domain: DomainId,
}
impl PersonalLocator {
    pub fn scope(&self, application: &str) -> Scope {
        Scope {
            mesh: self.mesh,
            app: ApplicationManifest::app_id(*self.publisher.as_bytes(), application),
            domain: self.domain,
        }
    }
}

pub struct DiscoveryCapability(Zeroizing<[u8; 32]>);
impl DiscoveryCapability {
    pub fn from_secret(mut secret: [u8; 32]) -> Self {
        let capability = Self(Zeroizing::new(secret));
        secret.zeroize();
        capability
    }
    pub fn topic(&self) -> &[u8; 32] {
        &self.0
    }
}
impl std::fmt::Debug for DiscoveryCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiscoveryCapability(<redacted>)")
    }
}

pub struct PersonalAccount {
    root: RootKey,
    namespace: PersonalNamespace,
    discovery: DiscoveryCapability,
    locator: PersonalLocator,
}
impl PersonalAccount {
    pub fn derive(
        prf: VerifiedPasskeyPrf,
        namespace: &PersonalNamespace,
    ) -> Result<Self, PrfUnsupported> {
        let input = namespace.prf_input().map_err(|_| PrfUnsupported)?;
        if prf.input != input || *prf.output == [0; 32] || *prf.output == input.0 {
            return Err(PrfUnsupported);
        }
        let root = RootKey::from_secret(*prf.output);
        drop(prf);
        let identity = root.identity();
        let canonical = namespace.canonical().map_err(|_| PrfUnsupported)?;
        let secret = Zeroizing::new(identity.secret());
        let discovery = DiscoveryCapability(Zeroizing::new(crate::group::hkdf(
            &[],
            &*secret,
            &[DISCOVERY, &canonical].concat(),
        )));
        let owner = identity.public_key();
        let locator = PersonalLocator {
            publisher: root.public_key(),
            owner,
            mesh: namespace.mesh(owner).map_err(|_| PrfUnsupported)?,
            domain: namespace.domain(owner).map_err(|_| PrfUnsupported)?,
        };
        Ok(Self {
            root,
            namespace: namespace.clone(),
            discovery,
            locator,
        })
    }
    pub fn root(&self) -> &RootKey {
        &self.root
    }
    pub fn identity(&self) -> IdentityKey {
        self.root.identity()
    }
    pub fn namespace(&self) -> &PersonalNamespace {
        &self.namespace
    }
    pub fn locator(&self) -> PersonalLocator {
        self.locator
    }
    pub fn discovery(&self) -> &DiscoveryCapability {
        &self.discovery
    }
    pub fn into_parts(self) -> (RootKey, DiscoveryCapability) {
        (self.root, self.discovery)
    }
}
impl std::fmt::Debug for PersonalAccount {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PersonalAccount")
            .field("locator", &self.locator)
            .field("namespace", &self.namespace)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
