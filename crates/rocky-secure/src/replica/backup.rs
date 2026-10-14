//! Coherent file copies for backups, and opening a copy under a fresh
//! signing identity. The copy keeps every operation, handle, ticket,
//! result and effect observation with its original identity; the new node
//! authors its own log and never signs as its predecessor. See
//! `docs/runtime/backup-and-restore.md`.
use super::generation::identity;
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_predecessors(node BLOB PRIMARY KEY,revision INTEGER NOT NULL)",
];

impl<A: RowApplication> EncryptedReplica<A> {
    /// Writes a consistent copy of the whole file to `target`, which must
    /// not exist, after flushing every commit. Writes wait for the copy;
    /// no transaction is held across anything else. Returns the revision.
    pub async fn backup_into(&self, target: &Path) -> Result<u64, Error> {
        if target.exists() {
            return Err(denied("the backup target exists"));
        }
        let target = target
            .to_str()
            .ok_or_else(|| denied("the backup target is not a UTF-8 path"))?
            .to_string();
        let mut inner = self.inner.lock().await;
        inner.flush()?;
        inner
            .conn
            .execute("VACUUM INTO ?", bindings(vec![SqlValue::Text(target)]))
            .await?;
        Ok(inner.revision)
    }
    /// Opens a copy of another node's file under `config.node`, a fresh
    /// signing identity. The copy must be of `predecessor`'s file; it is
    /// rebound once, then opens like any other. The predecessor's frames
    /// stay its own and are never signed again.
    pub async fn open_restored(
        path: impl AsRef<Path>,
        app: A,
        config: SecureConfig,
        upgrade: Option<Upgrade>,
        predecessor: PublicKey,
    ) -> Result<Self, Error> {
        let node = config.node.public_key();
        if node == predecessor {
            return Err(denied(
                "a restored file needs a fresh signing identity, not its predecessor's",
            ));
        }
        let publisher = config.publisher;
        let mut manifests = vec![
            config
                .manifest
                .verify(publisher)
                .map_err(credential)?
                .hash(),
        ];
        for step in upgrade.iter().flat_map(|upgrade| &upgrade.steps) {
            manifests.push(step.transition.verify(publisher).map_err(credential)?.from);
        }
        rebind(path.as_ref(), &manifests, predecessor, node).await?;
        Self::open_upgrading(path, app, config, upgrade).await
    }
    /// The signing identities this file was restored from, oldest first.
    pub async fn predecessors(&self) -> Result<Vec<PublicKey>, Error> {
        let inner = self.inner.lock().await;
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

/// Moves the file identity from `predecessor` to `node` for whichever of
/// `manifests` it is installed with. A file already bound to `node` is
/// left as it is.
async fn rebind(
    path: &Path,
    manifests: &[Commitment],
    predecessor: PublicKey,
    node: PublicKey,
) -> Result<(), Error> {
    check_replica_format(path).await?;
    let (owner, raw) = DatabaseOwner::open(path).await?;
    let conn = ScopedConnection::new(raw, None, crate::store::SCHEMA);
    let stored = rows(&conn, "SELECT identity FROM secure_metadata WHERE id=1").await?;
    let Some([SqlValue::Blob(stored)]) = stored.first().map(Vec::as_slice) else {
        return Err(denied("the restored file holds no replica"));
    };
    for manifest in manifests {
        let matches = |key| -> Result<bool, Error> { Ok(identity(*manifest, key)? == *stored) };
        if matches(node)? {
            return Ok(());
        }
        if !matches(predecessor)? {
            continue;
        }
        conn.execute("BEGIN IMMEDIATE", ()).await?;
        let result = async {
            for sql in SCHEMA {
                conn.execute(*sql, ()).await?;
            }
            conn.execute(
                "INSERT OR IGNORE INTO secure_predecessors VALUES(?,(SELECT revision FROM secure_metadata WHERE id=1))",
                [predecessor.as_bytes().to_vec()],
            )
            .await?;
            conn.execute(
                "UPDATE secure_metadata SET identity=?,revision=revision+1 WHERE id=1",
                [identity(*manifest, node)?],
            )
            .await?;
            Ok(())
        }
        .await;
        crate::storage::finish(&conn, result).await?;
        drop(owner);
        return Ok(());
    }
    Err(denied("the restored file is not the predecessor's"))
}
