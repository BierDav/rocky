/// What a store-only configuration cannot do.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StoreOnlyRefusal {
    /// Create or install a database, which needs the application's schema.
    NeedsSchema,
    /// Join with an invitation that grants readable access; a relay is invited with
    /// `Access::Store`.
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
