use crate::Commitment;

#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
pub struct TipId(#[serde(with = "crate::byte_arrays::one")] pub Commitment);
