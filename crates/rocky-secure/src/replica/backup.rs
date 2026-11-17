use super::generation::identity;
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_predecessors(node BLOB PRIMARY KEY,revision INTEGER NOT NULL)",
];

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn backup_into(&self, target: &Path) -> Result<u64, Error> {
        if rocky_platform::fs::exists(target) {
            return Err(denied("the backup target exists"));
        }
        let target = target
            .to_str()
            .ok_or_else(|| denied("the backup target is not a UTF-8 path"))?
            .to_string();
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner = flushed(inner, &self.inner).await?;
        inner
            .conn
            .execute("VACUUM INTO ?", bindings(vec![SqlValue::Text(target)]))
            .await?;
        Ok(inner.revision)
    }
    pub async fn open_restored(
        path: impl AsRef<Path>,
        app: A,
        config: impl Into<OpenConfig>,
        predecessor: PublicKey,
    ) -> Result<Self, Error> {
        let config = config.into();
        check_config_with_families(
            &config.security,
            &app.manifest(),
            &config.families(&app.manifest())?,
        )?;
        let node = config.security.node.public_key();
        if node == predecessor {
            return Err(denied(
                "a restored file needs a fresh signing identity, not its predecessor's",
            ));
        }
        rebind(
            path.as_ref(),
            predecessor,
            &config.security.node,
            config.security.publisher,
        )
        .await?;
        Self::open(path, app, config).await
    }
    pub async fn predecessors(&self) -> Result<Vec<PublicKey>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut found = Vec::new();
        for row in rows(
            &inner.conn,
            "SELECT node FROM secure_predecessors ORDER BY revision",
        )
        .await?
        {
            let [SqlValue::Blob(node)] = row.as_slice() else {
                return Err(Error::Corrupt("predecessor".into()));
            };
            let bytes: [u8; 32] = node
                .as_slice()
                .try_into()
                .map_err(|_| Error::Corrupt("predecessor".into()))?;
            found.push(
                PublicKey::from_bytes(&bytes).map_err(|_| Error::Corrupt("predecessor".into()))?,
            );
        }
        Ok(found)
    }
}

async fn rebind(
    path: &Path,
    predecessor: PublicKey,
    key: &PrivateKey,
    publisher: PublicKey,
) -> Result<(), Error> {
    let node = key.public_key();
    check_replica_format(path).await?;
    let (owner, raw) = DatabaseOwner::open(path).await?;
    let conn = ScopedConnection::new(raw, None, crate::store::SCHEMA);
    let stored = rows(&conn, "SELECT identity FROM secure_metadata WHERE id=1").await?;
    let Some([SqlValue::Blob(stored)]) = stored.first().map(Vec::as_slice) else {
        return Err(denied("the restored file holds no replica"));
    };
    const UNBIND_PREDECESSOR_LINE: &str = "DELETE FROM secure_line_nodes WHERE node=?";
    let (_, tip, bound): (String, TipId, PublicKey) = decode(stored)?;
    if bound == node {
        return Ok(());
    }
    if bound != predecessor {
        return Err(denied("backup belongs to another signing identity"));
    }
    {
        conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = async {
            for sql in SCHEMA {
                conn.execute(*sql, ()).await?;
            }
            super::identity::receipts::adopt_admission(&conn, publisher, predecessor, key).await?;
            conn.execute(
                "INSERT OR IGNORE INTO secure_predecessors VALUES(?,(SELECT revision FROM secure_metadata WHERE id=1))",
                [predecessor.as_bytes().to_vec()],
            )
            .await?;
            conn.execute(UNBIND_PREDECESSOR_LINE, [predecessor.as_bytes().to_vec()]).await?;
            conn.execute(
                "UPDATE secure_metadata SET identity=?,revision=revision+1 WHERE id=1",
                [identity(tip, node)?],
            )
            .await?;
            Ok(())
        }
        .await;
        crate::storage::finish(&conn, result).await?;
        drop(owner);
        Ok(())
    }
}
