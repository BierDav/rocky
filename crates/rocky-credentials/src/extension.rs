//! The versioned capability extension.
use crate::{Error, decode_bounded, denied, encode};
use serde::{Deserialize, Serialize};

const EXTENSION_VERSION: u32 = 13;
const MAX_EXTENSION: usize = 64 * 1024;

/// A capability's required, canonical versioned authority fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extension(Vec<u8>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    version: u32,
    node: Option<crate::PeerId>,
    partitions: Vec<crate::PartitionGrant>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    families: bool,
    issues: bool,
    trusted: Option<rocky_core::Scope>,
    claims: Option<crate::ClaimRight>,
    origin: Option<Origin>,
    sequence: Option<u64>,
    previous: Option<rocky_core::Commitment>,
    database: Option<DatabaseContext>,
    personal: Option<crate::PersonalBinding>,
}
/// Reads only the version, whatever else a later body holds.
#[derive(Deserialize)]
struct Versioned {
    version: u32,
}
/// What an extension carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extended {
    /// The node reference derived from the leaf subject key.
    pub node: Option<crate::PeerId>,
    /// Selector grants (extension version 2).
    pub partitions: Vec<crate::PartitionGrant>,
    /// Signed opt-in to the authenticated schema's `granted_like` families.
    /// Absent in existing per-table credentials, whose authority stays exact.
    pub families: bool,
    /// Issuer authority: the identity grants, renews, revokes and signs the
    /// domain's statements through this link.
    pub issues: bool,
    /// Owner-granted whole-domain tip and bookmark control; never delegated.
    pub trusted: Option<rocky_core::Scope>,
    /// Counter claims on the domain's coordinator (extension version 7);
    /// only on a short-lived link of its own.
    pub claims: Option<crate::ClaimRight>,
    /// The trusted identity that first enrolled this identity, kept by every
    /// renewal: a compromised root revocation of it ends this link too.
    pub origin: Option<Origin>,
    /// Membership sequence of a node certificate. The first link is zero.
    pub sequence: Option<u64>,
    /// The preceding membership certificate, or none for sequence zero.
    pub previous: Option<rocky_core::Commitment>,
    /// Stable database authority and namespace, independent of its schema.
    pub database: Option<DatabaseContext>,
    /// An independent owner device of a personal database, in place of a
    /// membership sequence.
    pub personal: Option<crate::PersonalBinding>,
}

/// The pinned root and stable namespace of one database.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseContext {
    pub publisher: crate::PublicKey,
    pub scope: rocky_core::Scope,
}
/// A trusted identity and when its root grant became valid: a recovery
/// ends every link whose origin's grant predates its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub identity: crate::PublicKey,
    /// The `not_before` of the origin's trusted grant.
    pub since: u64,
}
impl Extension {
    /// Canonical metadata bytes, for allocation checks before decoding.
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn new(extended: Extended) -> Result<Self, Error> {
        let body = Body {
            version: EXTENSION_VERSION,
            partitions: extended.partitions,
            families: extended.families,
            node: extended.node,
            issues: extended.issues,
            trusted: extended.trusted,
            claims: extended.claims,
            origin: extended.origin,
            sequence: extended.sequence,
            previous: extended.previous,
            database: extended.database,
            personal: extended.personal,
        };
        let extension = Self(encode(&body)?);
        extension.read()?;
        Ok(extension)
    }
    /// The body, refusing a version this runtime does not know.
    pub fn read(&self) -> Result<Extended, Error> {
        if self.0.len() > MAX_EXTENSION {
            return Err(denied("credential extension quota"));
        }
        let versioned: Versioned = p2panda_core::cbor::decode_cbor(self.0.as_slice())
            .map_err(|_| denied("invalid credential extension"))?;
        if versioned.version != EXTENSION_VERSION {
            return Err(denied("unsupported credential extension version"));
        }
        let body: Body = decode_bounded(&self.0, MAX_EXTENSION)?;
        if (body.sequence.is_none() != body.previous.is_none() && body.sequence != Some(0))
            || (body.personal.is_some() && (body.sequence.is_some() || body.previous.is_some()))
        {
            return Err(denied("invalid membership link"));
        }
        if body.partitions.len() > crate::MAX_GRANTS {
            return Err(denied("invalid partition credential extension"));
        }
        for grant in &body.partitions {
            grant.validate()?;
        }
        if let Some(claims) = &body.claims {
            claims.validate()?;
        }
        Ok(Extended {
            partitions: body.partitions,
            families: body.families,
            node: body.node,
            issues: body.issues,
            trusted: body.trusted,
            claims: body.claims,
            origin: body.origin,
            sequence: body.sequence,
            previous: body.previous,
            database: body.database,
            personal: body.personal,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn extensions_have_one_version_and_explicit_empty_authority() {
        let current = Extension::new(Extended::default()).unwrap();
        assert_eq!(current.read().unwrap(), Extended::default());
        let body: Body = decode_bounded(&current.0, MAX_EXTENSION).unwrap();
        assert_eq!(body.version, EXTENSION_VERSION);
        for version in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, u32::MAX] {
            let other = Extension(
                encode(&Body {
                    node: None,
                    version,
                    partitions: Vec::new(),
                    families: false,
                    issues: false,
                    trusted: None,
                    claims: None,
                    origin: None,
                    sequence: None,
                    previous: None,
                    database: None,
                    personal: None,
                })
                .unwrap(),
            );
            assert_eq!(
                other.read().unwrap_err().to_string(),
                "unsupported credential extension version"
            );
        }
        #[derive(Serialize)]
        struct MissingSelectors {
            version: u32,
        }
        let missing = Extension(
            encode(&MissingSelectors {
                version: EXTENSION_VERSION,
            })
            .unwrap(),
        );
        assert!(missing.read().is_err());
    }
}
