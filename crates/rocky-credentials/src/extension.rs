use crate::{Error, decode_bounded, denied, encode};
use serde::{Deserialize, Serialize};

const EXTENSION_VERSION: u32 = 20;
const MAX_EXTENSION: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Extension(#[serde(with = "serde_bytes")] Vec<u8>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    version: u32,
    node: Option<crate::PeerId>,
    partitions: Vec<crate::PartitionGrant>,
    whole: Option<crate::WholeAccess>,
    #[serde(with = "rocky_core::byte_arrays::one")]
    role: Option<rocky_core::Commitment>,
    issues: bool,
    trusted: Option<rocky_core::Scope>,
    owner: bool,
    origin: Option<Origin>,
    sequence: Option<u64>,
    #[serde(with = "rocky_core::byte_arrays::one")]
    previous: Option<rocky_core::Commitment>,
    database: Option<DatabaseContext>,
    possession: Option<crate::NodePossession>,
    line_signer: bool,
}
#[derive(Deserialize)]
struct Versioned {
    version: u32,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Extended {
    pub node: Option<crate::PeerId>,
    pub partitions: Vec<crate::PartitionGrant>,
    pub whole: Option<crate::WholeAccess>,
    pub role: Option<rocky_core::Commitment>,
    pub issues: bool,
    pub trusted: Option<rocky_core::Scope>,
    pub owner: bool,
    pub origin: Option<Origin>,
    pub sequence: Option<u64>,
    pub previous: Option<rocky_core::Commitment>,
    pub database: Option<DatabaseContext>,
    pub possession: Option<crate::NodePossession>,
    pub line_signer: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatabaseContext {
    pub publisher: crate::PublicKey,
    pub scope: rocky_core::Scope,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Origin {
    pub identity: crate::PublicKey,
    pub since: u64,
}
impl Extension {
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }

    pub fn new(extended: Extended) -> Result<Self, Error> {
        let body = Body {
            version: EXTENSION_VERSION,
            partitions: extended.partitions,
            whole: extended.whole,
            role: extended.role,
            node: extended.node,
            issues: extended.issues,
            trusted: extended.trusted,
            owner: extended.owner,
            origin: extended.origin,
            sequence: extended.sequence,
            previous: extended.previous,
            database: extended.database,
            possession: extended.possession,
            line_signer: extended.line_signer,
        };
        let extension = Self(encode(&body)?);
        extension.read()?;
        Ok(extension)
    }
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
            || (body.possession.is_some() && body.node.is_none())
        {
            return Err(denied("invalid membership link"));
        }
        if body.line_signer && body.possession.is_none() {
            return Err(denied("only a node certificate names a line signer"));
        }
        if body.owner && body.trusted.is_none() {
            return Err(denied("only a trusted grant names the owner"));
        }
        if body.partitions.len() > crate::MAX_GRANTS {
            return Err(denied("invalid partition credential extension"));
        }
        for grant in &body.partitions {
            grant.validate()?;
        }
        Ok(Extended {
            partitions: body.partitions,
            whole: body.whole,
            role: body.role,
            node: body.node,
            issues: body.issues,
            trusted: body.trusted,
            owner: body.owner,
            origin: body.origin,
            sequence: body.sequence,
            previous: body.previous,
            database: body.database,
            possession: body.possession,
            line_signer: body.line_signer,
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
        assert_eq!(EXTENSION_VERSION, 20);
        assert_eq!(body.version, EXTENSION_VERSION);
        for version in (0..EXTENSION_VERSION).chain([EXTENSION_VERSION + 1, u32::MAX]) {
            let other = Extension(
                encode(&Body {
                    node: None,
                    version,
                    partitions: Vec::new(),
                    whole: None,
                    role: None,
                    issues: false,
                    trusted: None,
                    owner: false,
                    origin: None,
                    sequence: None,
                    previous: None,
                    database: None,
                    possession: None,
                    line_signer: false,
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

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn only_a_trusted_grant_names_the_owner() {
        assert!(
            Extension::new(Extended {
                owner: true,
                ..Extended::default()
            })
            .is_err()
        );
        let scope = rocky_core::Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        let owner = Extended {
            trusted: Some(scope),
            owner: true,
            ..Extended::default()
        };
        assert_eq!(
            Extension::new(owner.clone()).unwrap().read().unwrap(),
            owner
        );
    }
}
