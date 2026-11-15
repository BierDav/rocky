//! A passkey's PRF output names one personal account: its root, the
//! root-derived owner identity and a discovery capability derived from the
//! identity secret. All of it lives in memory for one attempt; data keys
//! stay random and escrowed, never derived from the PRF.
use crate::{Error, IdentityKey, PersonalNamespace, PublicKey, RootKey};
use rocky_core::{ApplicationManifest, DomainId, MeshId, Scope};
use zeroize::{Zeroize, Zeroizing};

const PRF_INPUT: &str = "rocky.personal.prf-input.v1";
const DISCOVERY: &[u8] = b"rocky.personal.discovery.v1";

/// The deterministic WebAuthn PRF evaluation input (`eval.first`) of one
/// namespace. An empty device computes the same bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrfInput([u8; 32]);
impl PrfInput {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}
impl PersonalNamespace {
    /// What the passkey's PRF evaluates for this namespace.
    pub fn prf_input(&self) -> Result<PrfInput, Error> {
        Ok(PrfInput(rocky_core::commitment(
            PRF_INPUT,
            &self.canonical()?,
        )))
    }
}

/// A PRF output the trusted platform adapter verified: a discoverable
/// credential with required user verification answered a checked
/// challenge, origin and RP ID, and returned a real PRF result for
/// `input`. Rocky cannot prove any of that from 32 bytes; constructing it
/// is the adapter's claim. It is neither cloneable nor serializable, is
/// redacted in `Debug` and is erased when dropped.
///
/// ```compile_fail,E0599
/// use rocky_credentials::VerifiedPasskeyPrf;
/// fn copy(prf: VerifiedPasskeyPrf) -> [VerifiedPasskeyPrf; 2] {
///     [prf.clone(), prf]
/// }
/// ```
///
/// ```compile_fail,E0277
/// fn store<T: serde::Serialize>() {}
/// store::<rocky_credentials::VerifiedPasskeyPrf>();
/// ```
pub struct VerifiedPasskeyPrf {
    input: PrfInput,
    output: Zeroizing<[u8; 32]>,
}
impl VerifiedPasskeyPrf {
    /// `output` is the PRF result for `input`; the array is erased here.
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

/// The adapter's PRF is unusable for this namespace: it answered another
/// input, or returned no real PRF output. Nothing was derived or stored.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PrfUnsupported;
impl std::fmt::Display for PrfUnsupported {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("the passkey's PRF output is unavailable for this namespace")
    }
}
impl std::error::Error for PrfUnsupported {}

/// Where an empty device finds an owner's personal database: everything
/// public that the passkey determines.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PersonalLocator {
    /// The root's public key, the database's publisher.
    pub publisher: PublicKey,
    pub owner: PublicKey,
    pub mesh: MeshId,
    pub domain: DomainId,
}
impl PersonalLocator {
    /// The first-frame scope of the database whose schema names `application`.
    pub fn scope(&self, application: &str) -> Scope {
        Scope {
            mesh: self.mesh,
            app: ApplicationManifest::app_id(*self.publisher.as_bytes(), application),
            domain: self.domain,
        }
    }
}

/// The secret bootstrap discovery topic of one owner and namespace. It
/// derives the rendezvous keys too, so anyone holding it can track or
/// poison the topic; it grants no database authority. Redacted in `Debug`
/// and erased when dropped.
pub struct DiscoveryCapability(Zeroizing<[u8; 32]>);
impl DiscoveryCapability {
    /// A capability read back from a secret store.
    pub fn from_secret(mut secret: [u8; 32]) -> Self {
        let capability = Self(Zeroizing::new(secret));
        secret.zeroize();
        capability
    }
    /// The topic a discovery port announces under.
    pub fn topic(&self) -> &[u8; 32] {
        &self.0
    }
}
impl std::fmt::Debug for DiscoveryCapability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DiscoveryCapability(<redacted>)")
    }
}

/// One personal account, derived from a verified PRF for one namespace:
/// the root (`RootKey::from_secret(prf)`), its owner identity
/// (`RootKey::identity`) and the discovery capability (HKDF of the identity
/// secret under `rocky.personal.discovery.v1` and the canonical namespace).
/// Held for one interactive attempt; every secret is erased on drop.
pub struct PersonalAccount {
    root: RootKey,
    namespace: PersonalNamespace,
    discovery: DiscoveryCapability,
    locator: PersonalLocator,
}
impl PersonalAccount {
    /// Refuses a PRF evaluated for another input and an output that cannot
    /// be a PRF result (all zeros, or the input echoed back).
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
    /// The owner identity's key, derived again from the root.
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
    /// The root and the discovery capability, for an installation that
    /// consumes both.
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
