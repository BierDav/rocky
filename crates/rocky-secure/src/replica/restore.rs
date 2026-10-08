use super::*;

impl<A: RowApplication> Inner<A> {
    /// Applied operations of a scope in merge order: stamps and hashes only.
    /// Used once, for a file from before checkpoints, which the former quota
    /// bounds to 4,096 records.
    pub(super) async fn replay_order(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<(Stamp, Hash)>, Error> {
        let mut order = Vec::new();
        let mut cursor = Vec::new();
        loop {
            let records = collect_rows(self.conn.query(
                "SELECT hash,header FROM secure_operations WHERE scope=? AND status='applied' AND hash>? ORDER BY hash LIMIT 32",
                [encode(&scope)?, cursor.clone()],
            ).await?).await?;
            if records.is_empty() {
                break;
            }
            for record in records {
                let [SqlValue::Blob(hash), SqlValue::Blob(header)] = record.as_slice() else {
                    return Err(denied("corrupt replay record"));
                };
                cursor = hash.clone();
                let header: p2panda_core::Header<Extensions> = decode(header)?;
                order.push((
                    Stamp {
                        clock: header.timestamp,
                        author: header.public_key,
                        sequence: header.seq_num,
                    },
                    Hash::try_from(hash.as_slice()).map_err(|_| denied("corrupt replay record"))?,
                ));
            }
        }
        order.sort();
        Ok(order)
    }
}

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
