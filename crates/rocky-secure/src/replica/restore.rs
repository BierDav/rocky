use super::*;

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
