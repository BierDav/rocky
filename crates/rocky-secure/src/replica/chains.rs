//! Stored credential chains. Retained operations and the local node author
//! name a chain by digest; the chain is kept once
//! and removed with the last of them. See
//! `docs/decisions/0053-chains-by-reference.md`.
use super::*;
use crate::wire::Chain;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_chains(digest BLOB PRIMARY KEY,chain BLOB NOT NULL) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_operations_chain ON secure_operations(chain)",
];

impl<A: RowApplication> Inner<A> {
    /// Stores a chain a row of the same transaction is about to name.
    pub(super) async fn keep_chain(&self, chain: &Chain) -> Result<(), Error> {
        self.bind_node_author(chain.credentials()).await?;
        if self.chain_kept(chain.digest()).await? {
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT INTO secure_chains VALUES(?,?)",
                [chain.digest().to_vec(), encode(chain.credentials())?],
            )
            .await?;
        Ok(())
    }
    pub(super) async fn chain_kept(&self, digest: Commitment) -> Result<bool, Error> {
        let mut kept = self
            .conn
            .query(
                "SELECT 1 FROM secure_chains WHERE digest=?",
                [digest.to_vec()],
            )
            .await?;
        Ok(kept.next().await?.is_some())
    }
    /// The stored chain a retained operation names.
    pub(super) async fn stored_chain(&self, digest: Commitment) -> Result<Chain, Error> {
        let cached = || {
            self.chains
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
        };
        if let Some(chain) = cached().get(&digest) {
            return Ok(chain.clone());
        }
        let mut stored = self
            .conn
            .query(
                "SELECT chain FROM secure_chains WHERE digest=?",
                [digest.to_vec()],
            )
            .await?;
        let row = stored
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("a stored credential chain is missing".into()))?;
        let chain = Chain::from_bytes(&row.get::<Vec<u8>>(0)?)?;
        if chain.digest() != digest {
            return Err(Error::Corrupt("stored credential chain digest".into()));
        }
        let mut cached = cached();
        if cached.len() >= CACHED_CHAINS {
            cached.clear();
        }
        cached.insert(digest, chain.clone());
        Ok(chain)
    }
    /// A retained operation with the chain its header names.
    pub(crate) async fn stored_frame(
        &self,
        header: Vec<u8>,
        body: Vec<u8>,
    ) -> Result<EncryptedFrame, Error> {
        let frame = EncryptedFrame::bare(header, body);
        let chain = self.stored_chain(frame.chain_digest()?).await?;
        let mut frame = frame.with_chain(chain);
        frame.migration = self.migration_membership(frame.id()?).await?;
        Ok(frame)
    }
    /// Removes every chain nothing names any more. Runs in the transaction
    /// that removed operations, so no
    /// interruption leaves an operation without its chain.
    pub(super) async fn sweep_chains(&self) -> Result<(), Error> {
        let registered: Vec<Vec<u8>> = self
            .node_author
            .iter()
            .map(|author| author.chain.digest().to_vec())
            .collect();
        let unnamed = collect_rows(
            self.conn
                .query(
                    "SELECT digest FROM secure_chains c WHERE NOT EXISTS(SELECT 1 FROM secure_operations o WHERE o.chain=c.digest)",
                    (),
                )
                .await?,
        )
        .await?;
        for record in unnamed {
            let [SqlValue::Blob(digest)] = record.as_slice() else {
                return Err(Error::Corrupt("stored credential chain".into()));
            };
            if registered.contains(digest) {
                continue;
            }
            self.conn
                .execute("DELETE FROM secure_chains WHERE digest=?", [digest.clone()])
                .await?;
        }
        Ok(())
    }
}
#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    /// The digests of the stored chains, and how many retained operations
    /// name a chain that is not stored.
    pub(crate) async fn chain_census(&self) -> (BTreeSet<Commitment>, usize) {
        let inner = self.inner.lock().await;
        let stored = rows(&inner.conn, "SELECT digest FROM secure_chains")
            .await
            .unwrap()
            .into_iter()
            .map(|record| match record.as_slice() {
                [SqlValue::Blob(digest)] => digest.as_slice().try_into().unwrap(),
                _ => panic!("stored chain"),
            })
            .collect();
        let unchained = rows(
            &inner.conn,
            "SELECT hash FROM secure_operations o WHERE NOT EXISTS(SELECT 1 FROM secure_chains c WHERE c.digest=o.chain)",
        )
        .await
        .unwrap()
        .len();
        (stored, unchained)
    }
}
/// Decoded chains kept by digest; a replica names few distinct chains.
const CACHED_CHAINS: usize = 64;
