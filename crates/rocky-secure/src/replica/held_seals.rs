use super::*;
use rocky_core::Scope;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeldSeal {
    Checkpoint,
    Live,
}

impl<A: RowApplication> Inner<A> {
    async fn live_at(&self, domain: Scope, seal: Commitment) -> Result<bool, Error> {
        let Some(depth) = self.finalized_depth(domain, seal).await? else {
            return Ok(false);
        };
        for (scope, id) in self.domain_scopes(domain).await? {
            if !self.has_scope_key(scope) {
                continue;
            }
            let bound = match collect_rows(
                self.conn
                    .query(
                        "SELECT position FROM secure_seal_bounds WHERE scope=? AND depth=?",
                        [id, depth],
                    )
                    .await?,
            )
            .await?
            .first()
            .map(Vec::as_slice)
            {
                Some([SqlValue::Integer(position)]) => *position,
                None => match self.checkpoint_record(id, true).await? {
                    Some(record)
                        if record
                            .signed
                            .as_ref()
                            .and_then(|signed| signed.statement.seal)
                            == Some(seal) =>
                    {
                        record.position
                    }
                    _ => return Ok(false),
                },
                _ => return Err(Error::Corrupt("seal bound".into())),
            };
            let beyond = collect_rows(
                self.conn
                    .query(
                        "SELECT 1 FROM secure_applied WHERE scope=? AND position>? LIMIT 1",
                        [id, bound],
                    )
                    .await?,
            )
            .await?;
            if !beyond.is_empty() {
                return Ok(false);
            }
        }
        Ok(true)
    }
    pub(super) async fn held_seal(
        &self,
        domain: Scope,
        seal: Commitment,
    ) -> Result<Option<HeldSeal>, Error> {
        if self.live_at(domain, seal).await? {
            return Ok(Some(HeldSeal::Live));
        }
        let scopes = self.domain_scopes(domain).await?;
        let mut based = !scopes.is_empty();
        for (_, id) in scopes {
            based &= self.base_seal(id).await? == Some(seal)
                || self.available_pulled_seal(id).await? == Some(seal);
        }
        Ok(based.then_some(HeldSeal::Checkpoint))
    }
}

pub(super) fn bookmark_seal(
    graph: &rocky_credentials::TipGraph,
    tip: TipId,
) -> Result<Option<Commitment>, Error> {
    if let Some(seal) = graph.seal(tip).map_err(credential)? {
        return Ok(Some(seal.id().map_err(credential)?));
    }
    for seal in &graph.seals {
        if seal.successor().map_err(credential)? == Some(tip) {
            return Ok(Some(seal.id().map_err(credential)?));
        }
    }
    for opening in &graph.openings {
        if opening.tip().map_err(credential)? == tip {
            return Ok(Some(opening.statement.seal));
        }
    }
    Ok(None)
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn held_seal(
        &self,
        domain: Scope,
        seal: Commitment,
    ) -> Result<Option<HeldSeal>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.graph_for(domain)?;
        inner.held_seal(domain, seal).await
    }
}
