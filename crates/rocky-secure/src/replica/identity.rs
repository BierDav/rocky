//! Table author keys and transaction-local identity reservations.
use super::*;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_row_keys(scope BLOB NOT NULL,row BLOB NOT NULL,key BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_row_keys_key ON secure_row_keys(scope,key,row)",
    "CREATE TRIGGER IF NOT EXISTS secure_row_keys_delete AFTER DELETE ON secure_rows BEGIN DELETE FROM secure_row_keys WHERE scope=OLD.scope AND row=OLD.row; END",
];

pub(super) struct RowAuthor {
    pub(super) key: PrivateKey,
    pub(super) credentials: CredentialChain,
    pub(super) number: u32,
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Installs an externally held table pseudonym. Keys are not persisted;
    /// register them again after open or credential renewal.
    pub async fn register_row_author(
        &self,
        key: PrivateKey,
        credentials: CredentialChain,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        let (grants, partitions) = credentials
            .verify_selectors(
                inner.config.publisher,
                inner.manifest,
                key.public_key(),
                &inner.config.revocations,
                now()?,
            )
            .map_err(credential)?;
        let leaf = credentials
            .0
            .last()
            .ok_or_else(|| denied("missing pseudonym credential"))?;
        let number = credentials
            .authors()
            .map_err(credential)?
            .and_then(|range| range.single())
            .ok_or_else(|| denied("pseudonym requires one author number"))?;
        let table = grants
            .first()
            .map(|grant| grant.scope.table)
            .or_else(|| partitions.first().map(|grant| grant.table))
            .ok_or_else(|| denied("pseudonym requires a table"))?;
        let (local, local_partitions) = inner
            .config
            .credentials
            .verify_selectors(
                inner.config.publisher,
                inner.manifest,
                inner.config.node.public_key(),
                &inner.config.revocations,
                now()?,
            )
            .map_err(credential)?;
        if leaf.capability.kind != rocky_core::PrincipalKind::Pseudonym
            || !inner.tables.contains(&table)
            || grants.iter().any(|grant| {
                grant.scope.table != table || !local.iter().any(|held| held.includes(grant))
            })
            || partitions.iter().any(|grant| {
                grant.table != table || !local_partitions.iter().any(|held| held.includes(grant))
            })
        {
            return Err(denied("row author exceeds this device's authority"));
        }
        inner.row_authors.insert(
            table,
            RowAuthor {
                key,
                credentials,
                number,
            },
        );
        inner.process_pending(now()?).await?;
        Ok(())
    }
}
impl<A: RowApplication> Inner<A> {
    pub(super) fn storage_row(&self, change: &RowChange) -> Result<Commitment, Error> {
        Ok(
            match self
                .app
                .row_identity(change)
                .map_err(|message| denied(&message))?
            {
                Some(id) => {
                    rocky_core::commitment("rocky.row-incarnation.v2", &id.integer().to_le_bytes())
                }
                None => change.row,
            },
        )
    }
    pub(super) async fn key_row(
        &self,
        scope: ResourceScope,
        key: Commitment,
    ) -> Result<Option<Materialized>, Error> {
        if self.typed.is_some() {
            self.winner(scope, key).await
        } else {
            self.row(scope, key).await
        }
    }
    pub(super) async fn winner(
        &self,
        scope: ResourceScope,
        key: Commitment,
    ) -> Result<Option<Materialized>, Error> {
        let mut cursor = Vec::new();
        let mut winner: Option<Materialized> = None;
        loop {
            let chunk = collect_rows(self.conn.query(
                "SELECT r.row,r.fields FROM secure_row_keys k JOIN secure_rows r ON r.scope=k.scope AND r.row=k.row WHERE k.scope=? AND k.key=? AND k.row>? ORDER BY k.row LIMIT 256",
                [encode(&scope)?, key.to_vec(), cursor.clone()],
            ).await?).await?;
            for row in &chunk {
                let [SqlValue::Blob(id), SqlValue::Blob(bytes)] = row.as_slice() else {
                    return Err(Error::Corrupt("incarnation row".into()));
                };
                cursor = id.clone();
                let record: Materialized = decode(bytes)?;
                if !record.deleted
                    && record.key == Some(key)
                    && winner.as_ref().is_none_or(|held| {
                        (record.created.as_ref(), record.identity)
                            > (held.created.as_ref(), held.identity)
                    })
                {
                    winner = Some(record);
                }
            }
            if chunk.len() < 256 {
                return Ok(winner);
            }
        }
    }
    pub(super) async fn index_row_key(
        &self,
        scope: &[u8],
        row: &[u8],
        record: &Materialized,
    ) -> Result<(), Error> {
        if record.moved_from.is_some() && !record.deleted {
            self.conn
                .execute(
                    "INSERT OR IGNORE INTO secure_move_rows VALUES(?,?)",
                    [scope.to_vec(), row.to_vec()],
                )
                .await?;
        } else {
            self.conn
                .execute(
                    "DELETE FROM secure_move_rows WHERE scope=? AND row=?",
                    [scope.to_vec(), row.to_vec()],
                )
                .await?;
        }

        let key = record
            .key
            .map(|key| key.to_vec())
            .unwrap_or_else(|| row.to_vec());
        self.conn.execute(
            "INSERT INTO secure_row_keys VALUES(?,?,?) ON CONFLICT(scope,row) DO UPDATE SET key=excluded.key",
            [scope.to_vec(), row.to_vec(), key],
        ).await?;
        Ok(())
    }
    /// Backfills the derived index after open or a bulk restore, in bounded pages.
    pub(super) async fn index_row_keys(&self) -> Result<(), Error> {
        loop {
            let chunk = collect_rows(self.conn.query(
                "SELECT r.scope,r.row,r.fields FROM secure_rows r LEFT JOIN secure_row_keys k ON k.scope=r.scope AND k.row=r.row WHERE k.row IS NULL LIMIT 256",
                (),
            ).await?).await?;
            for row in &chunk {
                let [
                    SqlValue::Blob(scope),
                    SqlValue::Blob(row),
                    SqlValue::Blob(bytes),
                ] = row.as_slice()
                else {
                    return Err(Error::Corrupt("incarnation index row".into()));
                };
                self.index_row_key(scope, row, &decode(bytes)?).await?;
            }
            if chunk.len() < 256 {
                return Ok(());
            }
        }
    }
    /// Creation records outlive operation pruning, including deleted rows.
    /// This is a summary of the own creation log, not an allocation counter.
    pub(super) fn identity_allocation_scope(&self, scope: ResourceScope) -> ResourceScope {
        self.typed
            .as_ref()
            .and_then(|typed| {
                typed.by_scope(scope).and_then(|table| {
                    typed
                        .rows
                        .schema()
                        .table(&table.table.sql_name)
                        .filter(|logical| !logical.partition_by.is_empty())
                        .map(|_| table.scope)
                })
            })
            .unwrap_or(scope)
    }
    pub(super) async fn highest_identity(
        &self,
        scope: ResourceScope,
        author: u32,
    ) -> Result<u32, Error> {
        Ok(self
            .identity_marks(scope)
            .await?
            .into_iter()
            .find(|mark| mark.author == author)
            .map_or(0, |mark| mark.sequence))
    }
    pub(super) async fn identity_marks(
        &self,
        scope: ResourceScope,
    ) -> Result<Vec<checkpoint::IdentityMark>, Error> {
        let records = collect_rows(self.conn.query(
            "SELECT author,signer,sequence FROM secure_identity_marks WHERE scope=? ORDER BY author",
            [encode(&scope)?],
        ).await?).await?;
        records
            .into_iter()
            .map(|row| {
                let [
                    SqlValue::Integer(author),
                    SqlValue::Blob(signer),
                    SqlValue::Integer(sequence),
                ] = row.as_slice()
                else {
                    return Err(Error::Corrupt("row identity mark".into()));
                };
                Ok(checkpoint::IdentityMark {
                    author: u32::try_from(*author)
                        .map_err(|_| denied("invalid identity author"))?,
                    signer: PublicKey::from_bytes(
                        signer
                            .as_slice()
                            .try_into()
                            .map_err(|_| denied("invalid identity signer"))?,
                    )
                    .map_err(|error| denied(&error.to_string()))?,
                    sequence: u32::try_from(*sequence)
                        .map_err(|_| denied("invalid identity sequence"))?,
                })
            })
            .collect()
    }
    pub(super) async fn put_identity_mark(
        &self,
        scope: ResourceScope,
        mark: &checkpoint::IdentityMark,
    ) -> Result<(), Error> {
        if self
            .identity_marks(scope)
            .await?
            .iter()
            .any(|held| held.author == mark.author && held.signer != mark.signer)
        {
            return Err(denied(
                "checkpoint author number belongs to another signing key",
            ));
        }
        let allocation = self.identity_allocation_scope(scope);
        for scope in BTreeSet::from([scope, allocation]) {
            self.conn.execute(
            "INSERT INTO secure_identity_marks VALUES(?,?,?,?) ON CONFLICT(scope,author) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)",
            bindings(vec![SqlValue::Blob(encode(&scope)?), SqlValue::Integer(i64::from(mark.author)), SqlValue::Blob(mark.signer.as_bytes().to_vec()), SqlValue::Integer(i64::from(mark.sequence))]),
        ).await?;
        }
        Ok(())
    }
}

// Checkpoint row identities use the same canonical string boundary as callers.
pub(super) mod decimal {
    use serde::{Deserialize, Serialize};
    pub fn serialize<S: serde::Serializer>(
        identity: &Option<i64>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        identity
            .map(rocky_core::RowIdentity::from_integer)
            .transpose()
            .map_err(serde::ser::Error::custom)?
            .serialize(serializer)
    }
    pub fn deserialize<'de, D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<i64>, D::Error> {
        Ok(
            Option::<rocky_core::RowIdentity>::deserialize(deserializer)?
                .map(rocky_core::RowIdentity::integer),
        )
    }
}
