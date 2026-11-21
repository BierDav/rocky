use super::*;

pub(super) const TABLE: &str = "secure_authority_state";
pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_authority_state(key TEXT PRIMARY KEY,value BLOB NOT NULL,updated_at INTEGER NOT NULL) WITHOUT ROWID",
];

async fn table_exists(conn: &ScopedConnection) -> Result<bool, Error> {
    let found = rows(
        conn,
        "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='secure_authority_state'",
    )
    .await?;
    Ok(!found.is_empty())
}

async fn read_entry(conn: &ScopedConnection, key: &str) -> Result<Option<(Vec<u8>, u64)>, Error> {
    if !table_exists(conn).await? {
        return Ok(None);
    }
    let mut query = conn
        .query(
            "SELECT value,updated_at FROM secure_authority_state WHERE key=?",
            [key],
        )
        .await?;
    let Some(row) = query.next().await? else {
        return Ok(None);
    };
    let value = row.get::<Vec<u8>>(0)?;
    let updated_at = u64::try_from(row.get::<i64>(1)?)
        .map_err(|_| Error::Corrupt("authority state timestamp".into()))?;
    Ok(Some((value, updated_at)))
}

async fn apply_entries(
    conn: &ScopedConnection,
    entries: &[(&str, Option<&[u8]>)],
    updated_at: u64,
) -> Result<(), Error> {
    for sql in SCHEMA {
        conn.execute(*sql, ()).await?;
    }
    let updated_at = i64::try_from(updated_at)
        .map_err(|_| Error::Corrupt("authority state timestamp".into()))?;
    for (key, value) in entries {
        match value {
            Some(bytes) => {
                conn.execute(
                    "INSERT OR REPLACE INTO secure_authority_state(key,value,updated_at) VALUES(?,?,?)",
                    bindings(vec![
                        SqlValue::Text((*key).to_owned()),
                        SqlValue::Blob((*bytes).to_vec()),
                        SqlValue::Integer(updated_at),
                    ]),
                )
                .await?;
            }
            None => {
                conn.execute("DELETE FROM secure_authority_state WHERE key=?", [*key])
                    .await?;
            }
        }
    }
    Ok(())
}

pub fn read_authority_entry(path: &Path, key: &str) -> Result<Option<(Vec<u8>, u64)>, Error> {
    if !rocky_platform::fs::len(path).is_ok_and(|len| len > 0) {
        return Ok(None);
    }
    let mut wal = path.as_os_str().to_os_string();
    wal.push("-wal");
    let wal = std::path::PathBuf::from(wal);
    let mut shm = path.as_os_str().to_os_string();
    shm.push("-shm");
    let shm = std::path::PathBuf::from(shm);
    let had_sidecars = rocky_platform::fs::exists(&wal) || rocky_platform::fs::exists(&shm);
    let result = (|| {
        let conn = crate::sqlite::Connection::open_read_only(path)?;
        let found = conn.rows(
            "SELECT 1 FROM sqlite_schema WHERE type='table' AND name='secure_authority_state'",
            (),
        )?;
        if found.is_empty() {
            return Ok(None);
        }
        let found = conn.rows(
            "SELECT value,updated_at FROM secure_authority_state WHERE key=?",
            [key],
        )?;
        let Some(row) = found.into_iter().next() else {
            return Ok(None);
        };
        let [
            crate::sqlite::Value::Blob(value),
            crate::sqlite::Value::Integer(updated_at),
        ] = row.as_slice()
        else {
            return Err(Error::Corrupt("authority state".into()));
        };
        let updated_at = u64::try_from(*updated_at)
            .map_err(|_| Error::Corrupt("authority state timestamp".into()))?;
        Ok(Some((value.clone(), updated_at)))
    })();
    if !had_sidecars && rocky_platform::fs::len(&wal).is_ok_and(|len| len == 0) {
        let _ = rocky_platform::fs::remove_file(&wal);
        let _ = rocky_platform::fs::remove_file(&shm);
    }
    result
}

pub async fn write_authority_entry(
    path: &Path,
    key: &str,
    value: &[u8],
    updated_at: u64,
) -> Result<(), Error> {
    if let Some(parent) = path.parent() {
        rocky_platform::fs::create_dir_all(parent)
            .map_err(|error| Error::Corrupt(error.to_string()))?;
    }
    let (owner, raw) = DatabaseOwner::open(path).await?;
    let conn = ScopedConnection::new(raw, None, crate::store::SCHEMA);
    conn.execute("BEGIN IMMEDIATE", ()).await?;
    let result = apply_entries(&conn, &[(key, Some(value))], updated_at).await;
    crate::storage::finish(conn.connection(), result).await?;
    drop(owner);
    Ok(())
}

pub async fn remove_authority_entry(path: &Path, key: &str) -> Result<bool, Error> {
    if !rocky_platform::fs::len(path).is_ok_and(|len| len > 0) {
        return Ok(false);
    }
    let (owner, raw) = DatabaseOwner::open(path).await?;
    let conn = ScopedConnection::new(raw, None, crate::store::SCHEMA);
    let existed = read_entry(&conn, key).await?.is_some();
    if existed {
        conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = apply_entries(&conn, &[(key, None)], 0).await;
        crate::storage::finish(conn.connection(), result).await?;
    }
    drop(owner);
    Ok(existed)
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn get_authority_state(&self, key: &str) -> Result<Option<(Vec<u8>, u64)>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        read_entry(&inner.conn, key).await
    }
    pub async fn put_authority_state(
        &self,
        key: &str,
        value: &[u8],
        updated_at: u64,
    ) -> Result<(), Error> {
        self.commit_authority_state(&[(key, Some(value))], updated_at, None)
            .await?;
        Ok(())
    }
    pub async fn delete_authority_state(&self, key: &str) -> Result<(), Error> {
        self.commit_authority_state(&[(key, None)], 0, None).await?;
        Ok(())
    }
    pub async fn commit_authority_state(
        &self,
        entries: &[(&str, Option<&[u8]>)],
        updated_at: u64,
        revocations: Option<&RevocationSet>,
    ) -> Result<Vec<SignedRevocation>, Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let (added, should_settle) = inner
            .commit_authority_entries(entries, updated_at, revocations)
            .await?;
        drop(inner);
        if should_settle {
            self.settle().await?;
        }
        Ok(added)
    }
}

impl<A: RowApplication> Inner<A> {
    async fn commit_authority_entries(
        &mut self,
        entries: &[(&str, Option<&[u8]>)],
        updated_at: u64,
        revocations: Option<&RevocationSet>,
    ) -> Result<(Vec<SignedRevocation>, bool), Error> {
        let prepared = match revocations {
            Some(revocations) => {
                revocations
                    .verify_in(
                        self.config
                            .credentials
                            .database_context(self.config.publisher)
                            .map_err(credential)?,
                    )
                    .map_err(credential)?;
                let mut merged = self.config.revocations.clone();
                let added = merged
                    .merge(
                        revocations,
                        self.config.publisher,
                        self.definition.app,
                        now()?,
                    )
                    .map_err(credential)?;
                if merged == self.config.revocations {
                    Some((added, None))
                } else {
                    let authority_changed = !added.is_empty()
                        || merged.admissions().ne(self.config.revocations.admissions())
                        || self.config.revocations.certificates().any(|certificate| {
                            let node = certificate.capability.subject;
                            merged.ends_node_of(certificate.issuer, node, false)
                                != self.config.revocations.ends_node_of(
                                    certificate.issuer,
                                    node,
                                    false,
                                )
                        });
                    let membership_changed = merged
                        .certificates()
                        .ne(self.config.revocations.certificates());
                    let bytes = merged.to_bytes().map_err(credential)?;
                    Some((
                        added,
                        Some((merged, authority_changed, membership_changed, bytes)),
                    ))
                }
            }
            None => None,
        };
        let (added, revocation_update) = match prepared {
            Some((added, update)) => (added, update),
            None => (Vec::new(), None),
        };
        if revocation_update.is_none() {
            self.conn.execute("BEGIN IMMEDIATE", ()).await?;
            let result = apply_entries(&self.conn, entries, updated_at).await;
            crate::storage::finish(self.conn.connection(), result).await?;
            return Ok((added, false));
        }
        let old_graphs = self.graphs.clone();
        let old_tip = self.workspace.tip;
        let old_author = self.node_author.clone();
        let old_nodes = self.line_nodes.clone();
        let old_followed = self.workspace.followed.clone();
        let old_revocations = self.config.revocations.clone();
        let result = transaction!(self, |inner| {
            apply_entries(&inner.conn, entries, updated_at).await?;
            if let Some((merged, authority_changed, _, bytes)) = &revocation_update {
                let update = if *authority_changed {
                    "UPDATE secure_metadata SET revocations=?,revision=revision+1 WHERE id=1"
                } else {
                    "UPDATE secure_metadata SET revocations=? WHERE id=1"
                };
                inner.invalidate_context();
                inner.config.revocations = merged.clone();
                inner.graphs = old_graphs.clone();
                inner.workspace.tip = old_tip;
                inner.node_author = old_author.clone();
                inner.line_nodes = old_nodes.clone();
                inner.workspace.followed = old_followed.clone();
                inner.conn.execute(update, [bytes.clone()]).await?;
                if *authority_changed {
                    inner.resolve_revoked(merged).await?;
                    inner.drop_revoked_escrow(None).await?;
                    inner
                        .rejudge_reset_writers(&old_revocations, merged)
                        .await?;
                }
            }
            Ok(())
        });
        if let Err(error) = result {
            if revocation_update.is_some() {
                self.invalidate_context();
                self.config.revocations = old_revocations;
                self.graphs = old_graphs;
                self.workspace.tip = old_tip;
                self.node_author = old_author;
                self.line_nodes = old_nodes;
                self.workspace.followed = old_followed;
            }
            return Err(error);
        }
        let mut should_settle = false;
        if let Some((merged, authority_changed, membership_changed, _)) = revocation_update {
            self.invalidate_context();
            self.config.revocations = merged;
            if authority_changed {
                self.bump_authority();
                self.publish_revision().await?;
                should_settle = true;
            }
            if membership_changed {
                self.membership_events.send_replace(());
            }
        }
        Ok((added, should_settle))
    }
}
