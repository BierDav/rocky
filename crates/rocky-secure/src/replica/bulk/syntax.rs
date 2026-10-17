//! One owned successful parse, bound to the complete freshly selected blob.
use super::*;

const RAW_BYTES: usize = 32 * 1024;

struct ParsedDescriptor {
    hash: Hash,
    bytes: Vec<u8>,
    signed: SignedMigrationPublication,
}
#[derive(Default)]
pub(in crate::replica) struct StoredDescriptorSyntax {
    last: Option<ParsedDescriptor>,
    #[cfg(test)]
    capacity: Option<usize>,
    #[cfg(test)]
    parses: u64,
    #[cfg(test)]
    hits: u64,
}
impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) fn stored_migration_descriptor(
        &self,
        bytes: &[u8],
    ) -> Result<SignedMigrationPublication, Error> {
        let hash = {
            let memo = self
                .migration_syntax
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            #[cfg(test)]
            let mut memo = memo;
            #[cfg(test)]
            let capacity = memo.capacity.unwrap_or(RAW_BYTES).min(RAW_BYTES);
            #[cfg(not(test))]
            let capacity = RAW_BYTES;
            // Oversized input reaches the original parser before hashing or copying.
            let hash = (bytes.len() <= capacity).then(|| Hash::new(bytes));
            if let Some(hash) = hash
                && let Some(entry) = &memo.last
                && entry.hash == hash
                && entry.bytes.as_slice() == bytes
            {
                let signed = entry.signed.clone();
                #[cfg(test)]
                {
                    memo.hits += 1;
                }
                return Ok(signed);
            }
            #[cfg(test)]
            {
                memo.parses += 1;
            }
            hash
        };
        let signed = SignedMigrationPublication::from_bytes(bytes).map_err(credential)?;
        if let Some(hash) = hash {
            self.migration_syntax
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .last = Some(ParsedDescriptor {
                hash,
                bytes: bytes.to_vec(),
                signed: signed.clone(),
            });
        }
        Ok(signed)
    }
    #[cfg(test)]
    pub(crate) fn migration_syntax_capacity(&self, capacity: usize) {
        *self
            .migration_syntax
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = StoredDescriptorSyntax {
            capacity: Some(capacity),
            ..Default::default()
        };
    }
    #[cfg(test)]
    pub(crate) fn migration_syntax_stats(&self) -> (u64, u64, usize, usize) {
        let memo = self
            .migration_syntax
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        (
            memo.parses,
            memo.hits,
            usize::from(memo.last.is_some()),
            memo.last.as_ref().map_or(0, |e| e.bytes.len()),
        )
    }
    #[cfg(test)]
    pub(crate) fn migration_syntax_mismatch(&self) {
        let mut memo = self
            .migration_syntax
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let entry = memo.last.as_mut().unwrap();
        entry.bytes[0] ^= 1; // Same digest, unequal retained bytes: must miss.
        entry.signed.statement.bytes += 1;
    }
    #[cfg(test)]
    pub(crate) async fn classify_stored_migration_for_test(
        &self,
        statement: &MigrationPublicationStatement,
        frame: &EncryptedFrame,
    ) -> Result<(), Error> {
        self.classify_migration(statement, frame, &frame.migration.as_ref().unwrap().proof)
            .await
    }
    #[cfg(test)]
    pub(crate) async fn check_stored_migration_for_test(
        &self,
        frame: &EncryptedFrame,
        time: u64,
    ) -> Result<Option<MigrationPublicationStatement>, Error> {
        self.check_migration_frame(frame, time).await
    }
}
