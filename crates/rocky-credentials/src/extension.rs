//! Author ranges and the versioned capability extension.
use crate::{Error, decode_bounded, denied, encode};
use serde::{Deserialize, Serialize};

/// The highest author number: the `id` keeps 31 bits for it.
pub const MAX_AUTHOR: u32 = (1 << 31) - 1;
/// The root keeps the upper half of the author numbers for recoveries:
/// a domain's creator mints below it, so no trusted node hands one out.
pub const RECOVERY_FIRST: u32 = 1 << 30;
/// Author numbers of one recovery's trusted identity.
pub const RECOVERY_SLOT: u32 = 1 << 16;
/// Recoveries the reserved numbers hold.
pub const RECOVERY_SLOTS: u32 = (MAX_AUTHOR - RECOVERY_FIRST + 1) / RECOVERY_SLOT;
const EXTENSION_VERSION: u32 = 11;
const MAX_EXTENSION: usize = 64 * 1024;

/// Author numbers `first..=last`. Author 0 is reserved for the virtual row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthorRange {
    pub first: u32,
    pub last: u32,
}
impl AuthorRange {
    /// Every assignable author number.
    pub const ALL: Self = Self {
        first: 1,
        last: MAX_AUTHOR,
    };
    /// What a domain's creator is trusted with: every number below the
    /// recovery reserve.
    pub const DOMAIN: Self = Self {
        first: 1,
        last: RECOVERY_FIRST - 1,
    };
    /// The numbers of recovery `slot`, which only the root grants.
    pub fn recovery(slot: u32) -> Result<Self, Error> {
        if slot >= RECOVERY_SLOTS {
            return Err(denied("no such recovery slot"));
        }
        let first = RECOVERY_FIRST + slot * RECOVERY_SLOT;
        Self::new(first, first + (RECOVERY_SLOT - 1))
    }
    /// The recovery slot whose numbers include this range, if any.
    pub fn recovery_slot(&self) -> Option<u32> {
        let slot = self.first.checked_sub(RECOVERY_FIRST)? / RECOVERY_SLOT;
        Self::recovery(slot)
            .ok()
            .filter(|range| range.includes(self))
            .map(|_| slot)
    }
    pub fn new(first: u32, last: u32) -> Result<Self, Error> {
        let range = Self { first, last };
        if !range.valid() {
            return Err(denied("an author range is 1 to 2^31-1 and not empty"));
        }
        Ok(range)
    }
    fn valid(&self) -> bool {
        1 <= self.first && self.first <= self.last && self.last <= MAX_AUTHOR
    }
    pub fn contains(&self, author: u32) -> bool {
        (self.first..=self.last).contains(&author)
    }
    pub fn includes(&self, other: &Self) -> bool {
        self.first <= other.first && other.last <= self.last
    }
    /// A node's own number, when the range is exactly one.
    pub fn single(&self) -> Option<u32> {
        (self.first == self.last).then_some(self.first)
    }
}

/// A capability's required, canonical versioned authority fields.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extension(Vec<u8>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    version: u32,
    authors: Option<AuthorRange>,
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
}
/// Reads only the version, whatever else a later body holds.
#[derive(Deserialize)]
struct Versioned {
    version: u32,
}
/// What an extension carries.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extended {
    pub authors: Option<AuthorRange>,
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
    /// renewal: the root's revocation of it ends this link too.
    pub origin: Option<Origin>,
    /// Membership sequence of a node certificate. The first link is zero.
    pub sequence: Option<u64>,
    /// The preceding membership certificate, or none for sequence zero.
    pub previous: Option<rocky_core::Commitment>,
    /// Stable database authority and namespace, independent of its schema.
    pub database: Option<DatabaseContext>,
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
            authors: extended.authors,
            node: extended.node,
            issues: extended.issues,
            trusted: extended.trusted,
            claims: extended.claims,
            origin: extended.origin,
            sequence: extended.sequence,
            previous: extended.previous,
            database: extended.database,
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
        if body.authors.is_some_and(|authors| !authors.valid()) {
            return Err(denied("invalid credential extension"));
        }
        if body.sequence.is_none() != body.previous.is_none() && body.sequence != Some(0) {
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
            authors: body.authors,
            node: body.node,
            issues: body.issues,
            trusted: body.trusted,
            claims: body.claims,
            origin: body.origin,
            sequence: body.sequence,
            previous: body.previous,
            database: body.database,
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
        for version in [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, u32::MAX] {
            let other = Extension(
                encode(&Body {
                    node: None,
                    version,
                    authors: None,
                    partitions: Vec::new(),
                    families: false,
                    issues: false,
                    trusted: None,
                    claims: None,
                    origin: None,
                    sequence: None,
                    previous: None,
                    database: None,
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
