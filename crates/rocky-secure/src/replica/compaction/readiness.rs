//! Readiness includes authorized scopes before they have materialized history.
use super::*;

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) fn has_scope_key(&self, scope: ResourceScope) -> bool {
        self.config.keys.iter().any(|key| key.scope == scope)
    }
    pub(in crate::replica) async fn deferred_at(
        &self,
        scope: ResourceScope,
        seal: &SignedSeal,
    ) -> Result<bool, Error> {
        if self.has_scope_key(scope) {
            return Ok(false);
        }
        for record in collect_rows(
            self.conn
                .query(
                    "SELECT header FROM secure_operations WHERE scope=?",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?
        {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("frame header".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            let e = &header.extensions;
            if seal
                .statement
                .frontier
                .iter()
                .any(|mark| Some(mark.author) == e.author && e.author_sequence <= mark.sequence)
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub(in crate::replica) async fn readiness_scopes(
        &self,
        domain: Scope,
    ) -> Result<Vec<(ResourceScope, Option<i64>)>, Error> {
        let mut scopes = Vec::new();
        for scope in self.custody_scopes(now()?)? {
            if scope.domain == domain
                && self.tables.contains(&scope.table)
                && self.holds(scope).await?
                && self
                    .workspace
                    .typed
                    .as_ref()
                    .is_none_or(|typed| typed.declared_by_scope(scope).is_some())
                && !(self.partitioned(scope) && !self.partitions.contains_key(&scope))
            {
                scopes.push((scope, self.find_scope_id(scope).await?));
            }
        }
        Ok(scopes)
    }
    /// Materialized history only; observing a scope never creates its state.
    pub(in crate::replica) async fn domain_scopes(
        &self,
        domain: Scope,
    ) -> Result<Vec<(ResourceScope, i64)>, Error> {
        let mut scopes: Vec<_> = self
            .readiness_scopes(domain)
            .await?
            .into_iter()
            .filter_map(|(scope, id)| id.map(|id| (scope, id)))
            .collect();
        scopes.sort_unstable_by_key(|(_, id)| *id);
        Ok(scopes)
    }
    pub(in crate::replica) fn unverified_reason(scope: ResourceScope) -> String {
        format!("scope {scope:?} has no verified state at a seal of the path")
    }
    pub(in crate::replica) async fn unverified_scopes(
        &self,
        domain: Scope,
    ) -> Result<Vec<(ResourceScope, Option<i64>)>, Error> {
        let scopes = self.readiness_scopes(domain).await?;
        let last = self.finalized(domain).await?.last().cloned();
        let mut states = Vec::new();
        for (scope, id) in &scopes {
            let state = match id {
                // A pulled state counts once installed, never before.
                Some(id) => self.base_seal(*id).await?,
                None => None,
            };
            states.push((*scope, *id, state));
        }
        let has_base = states.iter().any(|(_, _, state)| state.is_some());
        let mut unverified = Vec::new();
        for (scope, id, state) in states {
            if let Some(seal) = self.path_seals(domain)?.last()
                && self.deferred_at(scope, seal).await?
            {
                continue;
            }
            if let Some(id) = id
                && self.has_seal_anchor(id).await?
            {
                continue;
            }
            let verified = match &last {
                Some(last) => {
                    let bound = match id {
                        Some(id) => self
                            .seal_bounds(id)
                            .await?
                            .iter()
                            .any(|(depth, _)| *depth == last.depth),
                        None => false,
                    };
                    let based = match state {
                        Some(seal) => match (
                            self.path_index(domain, seal)?,
                            self.path_index(domain, last.seal)?,
                        ) {
                            (Some(at), Some(last)) => at >= last,
                            (Some(_), None) => true,
                            _ => false,
                        },
                        None => false,
                    };
                    bound || based
                }
                None => state.is_some() || !has_base,
            };
            if !verified {
                unverified.push((scope, id));
            }
        }
        Ok(unverified)
    }
}
