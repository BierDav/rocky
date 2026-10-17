//! Exact original-node chain proof; no intake, replay or evidence writes.
use super::*;
use p2panda_core::{Header, validate_backlink};
mod records;

impl<A: RowApplication> Inner<A> {
    pub(super) async fn applied_restoration_endpoint(
        &self,
        scope: ResourceScope,
        id: i64,
        sequence: i64,
        hash: Hash,
    ) -> Result<bool, Error> {
        let applied = collect_rows(self.conn.query(
            "SELECT 1 FROM secure_operations o JOIN secure_applied a ON a.hash=o.hash WHERE o.hash=? AND o.scope=? AND o.author=? AND o.sequence=? AND o.status='applied' AND a.scope=?",
            bindings(vec![SqlValue::Blob(hash.as_bytes().to_vec()), SqlValue::Blob(encode(&scope)?), SqlValue::Blob(self.config.node.public_key().as_bytes().to_vec()), SqlValue::Integer(sequence), SqlValue::Integer(id)]),
        ).await?).await?;
        if applied.is_empty() {
            return Ok(false);
        }
        Ok(self
            .restoration_header(scope, id, sequence, hash)
            .await?
            .is_some())
    }

    pub(super) async fn restoration_dispositions(
        &self,
        scope: ResourceScope,
        id: i64,
        guarded: i64,
        endpoint: Hash,
        log: Option<checkpoint::Log>,
    ) -> Result<bool, Error> {
        let node = self.config.node.public_key();
        let anchor = match self.checkpoint_record(id, true).await? {
            Some(record) if !self.base_off_path(scope, id).await? => {
                let signed = self.verify_record(scope, &record)?;
                signed
                    .statement
                    .frontier
                    .into_iter()
                    .find(|point| point.author == node)
            }
            _ => None,
        };
        let log = log.map(|log| crate::LogPosition {
            author: node,
            sequence: log.sequence as u64,
            hash: log.hash,
            clock: log.clock,
        });
        let mut sequence = guarded;
        let mut hash = endpoint;
        let mut clock = None;
        for point in log.iter().chain(anchor.iter()) {
            if point.sequence > sequence as u64 {
                sequence = point.sequence as i64;
                hash = point.hash;
                clock = Some(point.clock);
            }
        }
        let mut child: Option<Header<Extensions>> = None;
        let mut passed = false;
        loop {
            if sequence == guarded {
                if hash != endpoint {
                    return Ok(false);
                }
                passed = true;
            }
            for point in log
                .iter()
                .chain(anchor.iter())
                .filter(|point| point.sequence == sequence as u64)
            {
                if point.hash != hash || clock.is_some_and(|clock| clock != point.clock) {
                    return Ok(false);
                }
            }
            if let Some(anchor) = anchor
                .as_ref()
                .filter(|anchor| anchor.sequence == sequence as u64 && sequence <= guarded)
            {
                if anchor.hash != hash || clock.is_some_and(|clock| clock != anchor.clock) {
                    return Ok(false);
                }
                if let Some(child) = &child
                    && (child.backlink != Some(anchor.hash)
                        || child.seq_num != anchor.sequence + 1
                        || child.timestamp <= anchor.clock)
                {
                    return Err(Error::Corrupt("restoration anchor link/clock".into()));
                }
                return Ok(passed);
            }
            let Some(header) = self.restoration_header(scope, id, sequence, hash).await? else {
                return Ok(false);
            };
            if clock.is_some_and(|clock| clock != header.timestamp) {
                return Err(Error::Corrupt("restoration endpoint clock".into()));
            }
            if let Some(child) = &child {
                validate_backlink(&header, child)
                    .map_err(|_| Error::Corrupt("restoration backlink".into()))?;
                if header.extensions.scope != child.extensions.scope
                    || header.timestamp >= child.timestamp
                {
                    return Err(Error::Corrupt("restoration scope/clock".into()));
                }
            }
            if sequence == 0 {
                if header.backlink.is_some() {
                    return Err(Error::Corrupt("restoration genesis backlink".into()));
                }
                return Ok(passed);
            }
            hash = header
                .backlink
                .ok_or_else(|| Error::Corrupt("restoration missing backlink".into()))?;
            sequence -= 1;
            clock = None;
            child = Some(header);
        }
    }
}
