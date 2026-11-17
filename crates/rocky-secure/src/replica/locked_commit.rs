use super::Inner;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct LockedCommit(u64);
impl LockedCommit {
    pub(crate) fn count(self) -> u64 {
        self.0
    }
}
impl<A> Inner<A> {
    pub(crate) fn locked_commit(&self) -> LockedCommit {
        LockedCommit(self.commit)
    }
}
