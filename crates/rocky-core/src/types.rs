use std::fmt;

pub const MAX_COUNTER: u64 = i64::MAX as u64;
pub const MAX_PAYLOAD_BYTES: usize = 65_536;

macro_rules! ids {
    ($($name:ident),+) => {$ (
        #[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
        #[derive(serde::Serialize, serde::Deserialize)]
        pub struct $name(#[serde(with = "crate::byte_arrays::one")] pub [u8; 16]);
    )+};
}
ids!(MeshId, AppId, DomainId, NodeId, PrincipalId);

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct Scope {
    pub mesh: MeshId,
    pub app: AppId,
    pub domain: DomainId,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProtocolError {
    InvalidManifest,
    InvalidValue,
    Truncated,
}
impl fmt::Display for ProtocolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ProtocolError {}
