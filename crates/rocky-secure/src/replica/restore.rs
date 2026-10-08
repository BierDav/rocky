use super::*;

/// Decrypts one applied frame's writes and checks they stay in the key's
/// scope. A bundle member replays alone: merging is order independent.
pub(super) fn replay_changes(
    key: &(ResourceScope, [u8; 32]),
    frame: &EncryptedFrame,
) -> Result<Vec<RowWrite>, Error> {
    let operation = frame.operation()?;
    let writes = RowPayload::decode(&decrypt(
        &key.1,
        &encode(&operation.header.extensions)?,
        &frame.body,
    )?)?
    .into_writes();
    if writes.iter().any(|write| write.change.scope != key.0) {
        return Err(denied("replay scope mismatch"));
    }
    Ok(writes)
}
