use super::*;
use rocky_credentials::{FenceLog, FenceScope, MAX_FENCE_LOGS};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ResetEndsSigner;
impl std::fmt::Display for ResetEndsSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("reset_ends_signer: the reset would end this device's own credentials")
    }
}
impl std::error::Error for ResetEndsSigner {}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn reset_fence(
        &self,
        keep: &BTreeSet<PeerId>,
    ) -> Result<Vec<FenceScope>, Error> {
        let mut scopes = BTreeSet::new();
        for sql in [
            "SELECT scope FROM secure_scope_state",
            "SELECT scope FROM secure_subscriptions",
        ] {
            for record in rows(&self.conn, sql).await? {
                let [SqlValue::Blob(scope)] = record.as_slice() else {
                    return Err(Error::Corrupt("scope".into()));
                };
                scopes.insert(decode::<ResourceScope>(scope)?);
            }
        }
        let mut fence = Vec::new();
        let mut logs = 0;
        for scope in scopes {
            let mut fenced: Vec<FenceLog> = self
                .frontier(scope)
                .await?
                .logs
                .into_iter()
                .filter(|log| log.applied > 0 && !keep.contains(&PeerId::of(log.author)))
                .map(|log| FenceLog {
                    writer: log.author,
                    next: log.applied,
                })
                .collect();
            if fenced.is_empty() {
                continue;
            }
            fenced.sort();
            logs += fenced.len();
            if logs > MAX_FENCE_LOGS {
                return Err(denied("reset fence exceeds its bound"));
            }
            fence.push(FenceScope {
                scope,
                logs: fenced,
            });
        }
        Ok(fence)
    }
    pub(super) async fn rejudge_reset_writers(
        &self,
        old: &RevocationSet,
        new: &RevocationSet,
    ) -> Result<(), Error> {
        let writers = rows(
            &self.conn,
            "SELECT DISTINCT author FROM secure_operations WHERE status='rejected'",
        )
        .await?;
        for record in writers {
            let [SqlValue::Blob(author)] = record.as_slice() else {
                return Err(Error::Corrupt("rejected author".into()));
            };
            let key = PublicKey::from_bytes(
                author
                    .as_slice()
                    .try_into()
                    .map_err(|_| Error::Corrupt("rejected author".into()))?,
            )
            .map_err(|_| Error::Corrupt("rejected author".into()))?;
            let node = PeerId::of(key);
            if old.reset_ends(&node) && old.ending_resets(&node) != new.ending_resets(&node) {
                self.conn
                    .execute(
                        "UPDATE secure_operations SET status='pending' WHERE author=? AND status='rejected'",
                        [author.clone()],
                    )
                    .await?;
            }
        }
        Ok(())
    }
}
