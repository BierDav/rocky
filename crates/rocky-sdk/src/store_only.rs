#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOnlyRefusal {
    NeedsSchema,
    ReadableAccess,
}
impl std::fmt::Display for StoreOnlyRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NeedsSchema => "a store-only configuration cannot create or install a database",
            Self::ReadableAccess => {
                "a store-only configuration opens only a keyless custody identity"
            }
        })
    }
}
impl std::error::Error for StoreOnlyRefusal {}
