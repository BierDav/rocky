use super::*;
use crate::wire::{Chain, RevokedHead};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_revoked_heads(hash BLOB PRIMARY KEY,scope BLOB NOT NULL,header BLOB NOT NULL,chain BLOB NOT NULL) WITHOUT ROWID",
];
pub(crate) const MAX_REVOKED_HEADS: usize = 64;

impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn missing_heads(&self, scope: ResourceScope) -> Result<Vec<Hash>, Error> {
        let inner = self.inner.lock(Class::Background).await;
        if !inner.config.revocations.has_revocations() {
            return Ok(Vec::new());
        }
        let pending = collect_rows(
            inner
                .conn
                .query(
                    "SELECT header FROM secure_operations WHERE scope=? AND status='pending' ORDER BY hash LIMIT ?",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Integer(MAX_REVOKED_HEADS as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        let mut missing = BTreeSet::new();
        for record in pending {
            let [SqlValue::Blob(header)] = record.as_slice() else {
                return Err(Error::Corrupt("pending operation".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(header)?;
            for id in &header.previous {
                if header.backlink != Some(*id)
                    && inner.stored_status(*id).await?.is_none()
                    && inner.stub(*id).await?.is_none()
                    && inner.kept_revoked_head(*id).await?.is_none()
                {
                    missing.insert(*id);
                }
            }
        }
        Ok(missing.into_iter().take(MAX_REVOKED_HEADS).collect())
    }
    pub(crate) async fn revoked_heads(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        heads: &[Hash],
    ) -> Result<Vec<RevokedHead>, Error> {
        if heads.len() > MAX_REVOKED_HEADS {
            return Err(denied("revoked head quota"));
        }
        let inner = self.inner.lock(Class::Background).await;
        inner.authorize_peer(peer, credentials, scope)?;
        if !inner.holds(scope).await? {
            return Err(denied("scope is not held by this replica"));
        }
        let mut found = Vec::new();
        for id in heads {
            if let Some(head) = inner.revoked_head(*id).await?
                && head.verify()?.extensions.scope == scope
                && inner.revoked_chain(head.chain())?
            {
                found.push(head);
            }
        }
        Ok(found)
    }
    pub(crate) async fn receive_revoked_heads(
        &self,
        scope: ResourceScope,
        heads: Vec<RevokedHead>,
    ) -> Result<(), Error> {
        if heads.len() > MAX_REVOKED_HEADS {
            return Err(denied("revoked head quota"));
        }
        let missing: BTreeSet<Hash> = self.missing_heads(scope).await?.into_iter().collect();
        let heads: Vec<_> = heads
            .into_iter()
            .filter_map(|head| {
                let header = head.verify().ok()?;
                (header.extensions.scope == scope && missing.contains(&header.hash()))
                    .then_some((header.hash(), head))
            })
            .collect();
        if heads.is_empty() {
            return Ok(());
        }
        {
            let mut inner = self.inner.lock(Class::Background).await;
            transaction!(inner, |inner| {
                for (id, head) in &heads {
                    inner
                        .conn
                        .execute(
                            "INSERT INTO secure_revoked_heads VALUES(?,?,?,?) ON CONFLICT(hash) DO NOTHING",
                            bindings(vec![
                                SqlValue::Blob(id.as_bytes().to_vec()),
                                SqlValue::Blob(encode(&scope)?),
                                SqlValue::Blob(head.header_bytes().to_vec()),
                                SqlValue::Blob(encode(head.chain().credentials())?),
                            ]),
                        )
                        .await?;
                }
                Ok(())
            })?;
        }
        self.settle().await
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) fn revoked_chain(&self, chain: &Chain) -> Result<bool, Error> {
        self.config
            .revocations
            .ends_chain(chain.credentials())
            .map_err(credential)
    }
    async fn revoked_head(&self, id: Hash) -> Result<Option<RevokedHead>, Error> {
        let stored = collect_rows(
            self.conn
                .query(
                    "SELECT header FROM secure_operations WHERE hash=?",
                    [id.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        if let Some([SqlValue::Blob(header)]) = stored.first().map(Vec::as_slice) {
            let chain = self
                .stored_chain(crate::wire::verified_header(header)?.extensions.chain)
                .await?;
            return Ok(Some(RevokedHead::new(header.clone(), chain)));
        }
        self.kept_revoked_head(id).await
    }
    async fn kept_revoked_head(&self, id: Hash) -> Result<Option<RevokedHead>, Error> {
        let kept = collect_rows(
            self.conn
                .query(
                    "SELECT header,chain FROM secure_revoked_heads WHERE hash=?",
                    [id.as_bytes().to_vec()],
                )
                .await?,
        )
        .await?;
        match kept.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(header), SqlValue::Blob(chain)]) => Ok(Some(RevokedHead::new(
                header.clone(),
                Chain::from_bytes(chain)?,
            ))),
            Some(_) => Err(Error::Corrupt("revoked head".into())),
            None => Ok(None),
        }
    }
    pub(super) async fn revoked_evidence(
        &self,
        header: &p2panda_core::Header<Extensions>,
        id: Hash,
    ) -> Result<bool, Error> {
        if header.backlink == Some(id) {
            return Ok(false);
        }
        let Some(head) = self.kept_revoked_head(id).await? else {
            return Ok(false);
        };
        let dependency = head.verify()?;
        Ok(dependency.extensions.scope == header.extensions.scope
            && dependency.timestamp < header.timestamp
            && self.revoked_chain(head.chain())?)
    }
}
