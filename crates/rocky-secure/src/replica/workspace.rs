use super::*;

pub(crate) struct Workspace {
    pub(crate) followed: Option<(rocky_core::Scope, String)>,
    pub(crate) tip: TipId,
    pub(super) typed: Option<Arc<typed::Typed>>,
    pub(crate) queries: queries::Queries,
}
impl Workspace {
    pub(super) fn new(tip: TipId) -> Self {
        Self {
            followed: None,
            tip,
            typed: None,
            queries: Default::default(),
        }
    }
}
