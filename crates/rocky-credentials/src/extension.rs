//! Author ranges and the versioned capability extension.
use crate::{Error, decode_bounded, denied, encode};
use serde::{Deserialize, Serialize};

/// The highest author number: the `id` keeps 31 bits for it.
pub const MAX_AUTHOR: u32 = (1 << 31) - 1;
const EXTENSION_VERSION: u32 = 5;
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
    issues: bool,
    trusted: Option<rocky_core::Scope>,
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
    /// Issuer authority: the identity grants, renews, revokes and signs the
    /// domain's statements through this link.
    pub issues: bool,
    /// Owner-granted whole-domain tip and bookmark control; never delegated.
    pub trusted: Option<rocky_core::Scope>,
}
impl Extension {
    pub fn new(extended: Extended) -> Result<Self, Error> {
        let body = Body {
            version: EXTENSION_VERSION,
            partitions: extended.partitions,
            authors: extended.authors,
            node: extended.node,
            issues: extended.issues,
            trusted: extended.trusted,
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
        if body.partitions.len() > crate::MAX_GRANTS {
            return Err(denied("invalid partition credential extension"));
        }
        for grant in &body.partitions {
            grant.validate()?;
        }
        Ok(Extended {
            partitions: body.partitions,
            authors: body.authors,
            node: body.node,
            issues: body.issues,
            trusted: body.trusted,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extensions_have_one_version_and_explicit_empty_authority() {
        let current = Extension::new(Extended::default()).unwrap();
        assert_eq!(current.read().unwrap(), Extended::default());
        let body: Body = decode_bounded(&current.0, MAX_EXTENSION).unwrap();
        assert_eq!(body.version, EXTENSION_VERSION);
        for version in [0, 1, 2, 3, 4, 6, u32::MAX] {
            let other = Extension(
                encode(&Body {
                    node: None,
                    version,
                    authors: None,
                    partitions: Vec::new(),
                    issues: false,
                    trusted: None,
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
