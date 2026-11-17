use crate::{Blocker, BlockerKind, ClearsBy, Error};

pub(crate) fn named<'a>(
    blockers: &'a [Blocker],
    scope: &str,
    kind: impl Fn(&BlockerKind) -> bool,
) -> Option<&'a Blocker> {
    blockers
        .iter()
        .find(|blocker| blocker.scope == scope && kind(&blocker.kind))
}
pub(crate) fn assert_named(
    blockers: &[Blocker],
    scope: &str,
    kind: impl Fn(&BlockerKind) -> bool,
    clears_by: ClearsBy,
) {
    let found = named(blockers, scope, kind);
    assert!(
        found.is_some_and(|blocker| blocker.clears_by == clears_by),
        "{scope} {clears_by:?} in {blockers:?}"
    );
}
pub(crate) fn refused<T>(result: Result<T, Error>) -> Vec<Blocker> {
    match result {
        Err(Error::Blocked(blockers)) => blockers,
        Err(error) => panic!("refused without blockers: {error}"),
        Ok(_) => panic!("not refused"),
    }
}
