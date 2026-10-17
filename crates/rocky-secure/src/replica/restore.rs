use super::*;

/// Checks every original write stays in the replay scope before filtering.
/// A bundle member replays alone: merging is order independent.
pub(super) fn replay_changes(
    scope: ResourceScope,
    payload: &RowPayload,
) -> Result<&[RowWrite], Error> {
    let writes = payload.writes();
    if writes.iter().any(|write| write.change.scope != scope) {
        return Err(denied("replay scope mismatch"));
    }
    Ok(writes)
}
