use rocky_core::{Commitment, CoordinationError};
use serde::{Deserialize, Serialize};
pub const MAX_NAME: usize = 64;
pub const CLAIM_WINDOW: u64 = 7 * 86_400;
pub const CLAIM_RECORDS: usize = 1024;
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(try_from = "Vec<u8>", into = "Vec<u8>")]
pub struct Name(Vec<u8>);
impl Name {
    pub fn new(name: impl Into<Vec<u8>>) -> Result<Self, CoordinationError> {
        let name = name.into();
        if name.is_empty() || name.len() > MAX_NAME {
            return Err(CoordinationError::Configuration);
        }
        Ok(Self(name))
    }
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}
impl TryFrom<Vec<u8>> for Name {
    type Error = CoordinationError;
    fn try_from(name: Vec<u8>) -> Result<Self, Self::Error> {
        Self::new(name)
    }
}
impl From<Name> for Vec<u8> {
    fn from(name: Name) -> Self {
        name.0
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Claim {
    pub id: Commitment,
    pub issued: u64,
}
impl Claim {
    pub fn fresh() -> Result<Self, CoordinationError> {
        Ok(Self {
            id: rocky_platform::entropy::random(),
            issued: rocky_platform::time::SystemTime::now()
                .duration_since(rocky_platform::time::UNIX_EPOCH)
                .map_err(|_| CoordinationError::Corrupt)?
                .as_secs(),
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reading {
    pub value: u64,
    pub claim: Option<Lookup>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Lookup {
    Claimed(u64),
    Unclaimed,
    Expired,
}
