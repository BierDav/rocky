use super::*;

fn covers<T: Serialize>(new: &[T], old: &[T]) -> Result<bool, Error> {
    let retained = new.iter().map(encode).collect::<Result<BTreeSet<_>, _>>()?;
    for item in old {
        if !retained.contains(&encode(item)?) {
            return Ok(false);
        }
    }
    Ok(true)
}

impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Deletes only a redundant custody archive covered byte-for-byte by an
    /// already durable successor. Row history, jobs and deduplication stay pinned.
    pub async fn prune_redundant_recovery(
        &self,
        obsolete: Commitment,
        successor: Commitment,
        key: &[u8; 32],
    ) -> Result<bool, Error> {
        if obsolete == successor {
            return Err(denied("retention successor must differ"));
        }
        let mut inner = self.inner.lock().await;
        let mut packages = Vec::new();
        for id in [obsolete, successor] {
            let mut records = inner
                .conn
                .query(
                    "SELECT package FROM secure_recovery_custody WHERE id=?",
                    [id.to_vec()],
                )
                .await?;
            let Some(record) = records.next().await? else {
                if id == obsolete {
                    return Ok(false);
                }
                return Err(denied("retention successor custody unavailable"));
            };
            let package = RecoveryPackage::from_bytes(&record.get::<Vec<u8>>(0)?)?;
            if package.commitment()? != id {
                return Err(denied("custody archive binding mismatch"));
            }
            package.inspect(inner.config.publisher, &inner.config.revocations)?;
            authorize(&inner.local_grants(now()?)?, &package.inventory.scopes)?;
            if package.inventory.recipient != inner.config.node.public_key()
                || package.inventory.definition != inner.manifest
            {
                return Err(denied("retention archive recipient mismatch"));
            }
            packages.push(package);
        }
        let old = &packages[0];
        let new = &packages[1];
        if old.exporter != new.exporter
            || old.inventory.schema != new.inventory.schema
            || old.inventory.handler != new.inventory.handler
            || old.inventory.revision > new.inventory.revision
            || old
                .inventory
                .scopes
                .iter()
                .any(|scope| !new.inventory.scopes.contains(scope))
            || old.inventory.key_epochs.iter().any(|(scope, epoch)| {
                new.inventory
                    .key_epochs
                    .get(scope)
                    .is_none_or(|next| !epoch.is_subset(next))
            })
        {
            return Err(denied("retention successor is incompatible"));
        }
        let old = old.open(key)?;
        let new = new.open(key)?;
        if !covers(&new.frames, &old.frames)?
            || !covers(&new.results, &old.results)?
            || !covers(&new.preparations, &old.preparations)?
            || !covers(&new.commands, &old.commands)?
            || !covers(&new.handles, &old.handles)?
            || !covers(&new.observations, &old.observations)?
            || !covers(&new.durability, &old.durability)?
        {
            return Err(denied("retention successor omits pinned recovery data"));
        }
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "DELETE FROM secure_recovery_custody WHERE id=?",
                    [obsolete.to_vec()],
                )
                .await?;
            Ok(())
        })?;
        Ok(true)
    }
}
