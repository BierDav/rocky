//! Existence logs of tables another reader scope may reference: one signed
//! record per row creation or deletion, with the row's `id` and no values,
//! verifiable by every device of the domain without the scope key. See
//! `docs/runtime/existence-logs.md`.
mod archive;
mod base;
mod seal;
use super::*;
pub use base::{ExistenceBaseAttachment, ExistenceBasePage, ExistenceBasis};
use p2panda_core::Signature;
use rocky_core::RowIdentity;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_existence_replay(scope BLOB NOT NULL,signer BLOB NOT NULL,sequence INTEGER NOT NULL,record BLOB NOT NULL,PRIMARY KEY(scope,signer,sequence)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_existence_archive(base BLOB PRIMARY KEY,scope BLOB NOT NULL,attachment BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_existence_archive_rows(base BLOB NOT NULL,id INTEGER NOT NULL,created BLOB,deleted BLOB,PRIMARY KEY(base,id)) WITHOUT ROWID",
    // Each record's frame position and row id, to find what a switch changes.
    "CREATE TABLE IF NOT EXISTS secure_existence_log(scope BLOB NOT NULL,signer BLOB NOT NULL,sequence INTEGER NOT NULL,hash BLOB NOT NULL UNIQUE,record BLOB NOT NULL,author INTEGER NOT NULL,position INTEGER NOT NULL,id INTEGER NOT NULL,PRIMARY KEY(scope,signer,sequence)) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_existence_log_position ON secure_existence_log(scope,author,position)",
    "CREATE INDEX IF NOT EXISTS secure_existence_log_id ON secure_existence_log(scope,id)",
    "CREATE TABLE IF NOT EXISTS secure_existence_base(scope BLOB PRIMARY KEY,attachment BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_existence_base_rows(scope BLOB NOT NULL,id INTEGER NOT NULL,created BLOB,deleted BLOB,PRIMARY KEY(scope,id)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_existence_stage(scope BLOB PRIMARY KEY,attachment BLOB NOT NULL,cursor INTEGER NOT NULL,complete INTEGER NOT NULL,count INTEGER NOT NULL,digest BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_existence_stage_rows(scope BLOB NOT NULL,id INTEGER NOT NULL,created BLOB,deleted BLOB,PRIMARY KEY(scope,id)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_existence(scope BLOB NOT NULL,id INTEGER NOT NULL,created BLOB,deleted BLOB,PRIMARY KEY(scope,id)) WITHOUT ROWID",
];

/// The format of existence records this runtime reads and writes.
pub const EXISTENCE_FORMAT: u8 = 4;
/// Records per existence page.
pub const MAX_EXISTENCE_PAGE: usize = 256;
const SIGNATURE: &str = "rocky.existence-record.v2";
const HASH: &str = "rocky.existence-record.hash.v2";

/// What one existence record states about its row.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExistenceEvent {
    Created,
    Deleted,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Statement {
    version: u8,
    tip: rocky_core::TipId,
    scope: ResourceScope,
    id: RowIdentity,
    event: ExistenceEvent,
    /// The node's author number.
    author: u32,
    author_sequence: u64,
    /// Position in the signer's log of this scope, from 1.
    sequence: u64,
    previous: Option<Commitment>,
    credentials: CredentialChain,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    partition: Option<rocky_core::PartitionTuple>,
}

/// A signed record of one row creation or deletion in an existence log.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExistenceRecord {
    statement: Statement,
    signer: PublicKey,
    signature: Signature,
}
impl ExistenceRecord {
    /// The tip this record belongs to.
    pub fn tip(&self) -> rocky_core::TipId {
        self.statement.tip
    }
    pub fn scope(&self) -> ResourceScope {
        self.statement.scope
    }
    pub fn id(&self) -> i64 {
        self.statement.id.integer()
    }
    pub fn event(&self) -> ExistenceEvent {
        self.statement.event
    }
    pub fn signer(&self) -> PublicKey {
        self.signer
    }
    pub fn sequence(&self) -> u64 {
        self.statement.sequence
    }
    pub fn hash(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(HASH, &encode(self)?))
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > SECURE_MESSAGE_BYTES - 1024 {
            return Err(Error::Quota {
                resource: "existence record",
                limit: SECURE_MESSAGE_BYTES - 1024,
            });
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        let record: Self = decode(bytes)?;
        if record.statement.version != EXISTENCE_FORMAT {
            return Err(Error::Incompatible(
                "unsupported existence record format; upgrade the runtime".into(),
            ));
        }
        Ok(record)
    }
    /// The signature over the statement under the signer's key.
    fn signed(&self) -> Result<bool, Error> {
        Ok(self
            .signer
            .verify(&signing_bytes(SIGNATURE, &self.statement)?, &self.signature))
    }
    /// Signs an arbitrary statement, as a misbehaving author could.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn forge(
        key: &PrivateKey,
        credentials: CredentialChain,
        tip: rocky_core::TipId,
        scope: ResourceScope,
        id: i64,
        event: ExistenceEvent,
        author: u32,
        sequence: u64,
        previous: Option<Commitment>,
    ) -> Self {
        let statement = Statement {
            version: EXISTENCE_FORMAT,
            tip,
            scope,
            id: RowIdentity::from_integer(id).expect("test identity"),
            event,
            author,
            author_sequence: sequence,
            sequence,
            previous,
            credentials,
            partition: None,
        };
        Self {
            signature: key.sign(&signing_bytes(SIGNATURE, &statement).expect("encode")),
            statement,
            signer: key.public_key(),
        }
    }
    #[cfg(test)]
    pub(crate) fn tampered(mut self, id: i64) -> Self {
        self.statement.id = RowIdentity::from_integer(id).expect("test identity");
        self
    }
}

/// Records of one existence log after a position, in signer and sequence order.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ExistencePage {
    pub records: Vec<ExistenceRecord>,
    pub next: Option<(PublicKey, u64)>,
}

impl<A: RowApplication> Inner<A> {
    /// The installed scope of a table with an existence log.
    pub(super) fn existence_table(&self, scope: ResourceScope) -> bool {
        self.workspace.typed.as_ref().is_some_and(|typed| {
            typed.declared_by_scope(scope).is_some_and(|projected| {
                typed
                    .rows
                    .schema()
                    .table(&projected.table.sql_name)
                    .is_some_and(|table| table.existence_log)
            })
        })
    }
    /// Signs the creations and deletions of `writes`, all in `scope`, with
    /// the node, inside the caller's transaction and before the
    /// rows are projected.
    pub(super) async fn author_existence(
        &self,
        scope: ResourceScope,
        writes: &[RowWrite],
        author_sequence: u64,
    ) -> Result<(), Error> {
        if !self.existence_table(scope) {
            return Ok(());
        }
        let author = self
            .node_author
            .as_ref()
            .ok_or_else(|| denied("node author credential missing"))?;
        let signer = author.key.public_key();
        let (mut sequence, mut previous) = self.existence_tip(scope, signer).await?;
        for write in writes {
            let event = match (&write.change.before, &write.change.after) {
                (None, Some(_)) => ExistenceEvent::Created,
                (Some(_), None) => ExistenceEvent::Deleted,
                _ => continue,
            };
            let id = self
                .app
                .row_identity(&write.change)
                .map_err(|message| denied(&message))?
                .ok_or_else(|| denied("an existence log needs row identities"))?;
            sequence += 1;
            let partition = self
                .app
                .row_partition(&write.change)
                .map_err(|error| denied(&error))?;
            let statement = Statement {
                version: EXISTENCE_FORMAT,
                tip: self.workspace.tip,
                scope,
                id,
                event,
                author: author.number,
                author_sequence,
                sequence,
                previous,
                credentials: author.credentials.clone(),
                partition,
            };
            let record = ExistenceRecord {
                signature: author.key.sign(&signing_bytes(SIGNATURE, &statement)?),
                statement,
                signer,
            };
            previous = Some(record.hash()?);
            if self.store_existence(&record).await? != ReceiveStatus::Applied {
                return Err(Error::Corrupt("local existence record".into()));
            }
        }
        Ok(())
    }
    async fn existence_tip(
        &self,
        scope: ResourceScope,
        signer: PublicKey,
    ) -> Result<(u64, Option<Commitment>), Error> {
        let found = rows_with(
            &self.conn,
            "SELECT sequence,hash FROM secure_existence_log WHERE scope=? AND signer=? ORDER BY sequence DESC LIMIT 1",
            vec![
                SqlValue::Blob(encode(&scope)?),
                SqlValue::Blob(signer.as_bytes().to_vec()),
            ],
        )
        .await?;
        match found.first().map(Vec::as_slice) {
            None => match self
                .existence_base(scope)
                .await?
                .as_ref()
                .map(|base| base.cut())
                .transpose()?
                .and_then(|cut| cut.logs.iter().find(|log| log.signer == signer))
            {
                Some(log) => Ok((log.sequence, Some(log.head))),
                None => Ok((0, None)),
            },
            Some([SqlValue::Integer(sequence), SqlValue::Blob(hash)]) => Ok((
                *sequence as u64,
                Some(hash.as_slice().try_into().map_err(|_| corrupt())?),
            )),
            Some(_) => Err(corrupt()),
        }
    }
    /// Checks everything a record states without the scope key: format,
    /// context, signature, the author's credential chain and its insert or
    /// delete right, and that a creation's `id` belongs to its author.
    fn verify_existence(&self, record: &ExistenceRecord, time: u64) -> Result<(), Error> {
        let statement = &record.statement;
        if statement.version != EXISTENCE_FORMAT {
            return Err(Error::Incompatible(
                "unsupported existence record format; upgrade the runtime".into(),
            ));
        }
        self.admits(self.manifest_of_tip(statement.tip)?)?;
        if let Some(graph) = self.graphs.get(&statement.scope.domain) {
            if !self.visible_frame(
                graph,
                statement.tip,
                statement.author,
                statement.author_sequence,
                self.workspace.tip,
            )? {
                return Err(denied("existence record belongs to an unfollowed tip"));
            }
        } else if statement.tip != self.workspace.tip {
            return Err(denied("existence record belongs to an unfollowed tip"));
        }

        let scope = statement.scope;
        if scope.domain.app != self.definition.app
            || !self.domains.contains(&scope.domain.domain)
            || !self.existence_table(scope)
        {
            return Err(denied(
                "existence record outside an installed existence log",
            ));
        }
        if statement.sequence == 0
            || statement.sequence > i64::MAX as u64
            || statement.author_sequence > i64::MAX as u64
        {
            return Err(denied("invalid existence log position"));
        }
        if !record.signed()? {
            return Err(denied("invalid existence record signature"));
        }
        let revocations = self.revocations_for(
            self.manifest_of_tip(statement.tip)?,
            &self.config.revocations,
        )?;
        self.check_authenticated_partition(scope, statement.partition.as_ref())?;
        let grants = if let Some(tuple) = &statement.partition {
            statement
                .credentials
                .verify_partition_author(
                    self.config.publisher,
                    record.signer,
                    statement.author,
                    scope,
                    tuple,
                    revocations,
                    time,
                )
                .map_err(credential)?
        } else {
            statement
                .credentials
                .verify_author(
                    self.config.publisher,
                    record.signer,
                    statement.author,
                    scope.table,
                    revocations,
                    time,
                )
                .map_err(credential)?
        };
        let right = match statement.event {
            ExistenceEvent::Created => Right::Insert,
            ExistenceEvent::Deleted => Right::Delete,
        };
        if !grants
            .iter()
            .any(|grant| grant.scope == scope && grant.rights.contains(&right))
        {
            return Err(denied("existence record author lacks the right"));
        }
        if statement.event == ExistenceEvent::Created && statement.id.author() != statement.author {
            return Err(denied("a created id belongs to another author"));
        }
        Ok(())
    }
    /// Stores a verified record. A gap waits for its predecessor; a second
    /// record at one position or a second creation of one `id` is refused.
    async fn store_existence(&self, record: &ExistenceRecord) -> Result<ReceiveStatus, Error> {
        let statement = &record.statement;
        self.bind_node_author(&statement.credentials).await?;
        let scope = SqlValue::Blob(encode(&statement.scope)?);
        let signer = SqlValue::Blob(record.signer.as_bytes().to_vec());
        let hash = record.hash()?;
        let base = self.existence_base(statement.scope).await?;
        let covered = base
            .as_ref()
            .map(|base| base.cut())
            .transpose()?
            .and_then(|cut| cut.logs.iter().find(|log| log.signer == record.signer));
        if let Some(log) = covered
            && statement.sequence <= log.sequence
        {
            if statement.sequence == log.sequence && hash != log.head {
                return Err(denied("existence record conflicts with signed base head"));
            }
            return Ok(ReceiveStatus::Pruned);
        }
        if statement.event == ExistenceEvent::Created
            && let Some(mark) = base
                .as_ref()
                .map(|base| base.descriptor())
                .transpose()?
                .and_then(|base| {
                    base.identities
                        .iter()
                        .find(|mark| mark.author == statement.id.author())
                })
            && (mark.signer != record.signer || statement.id.sequence() <= mark.sequence)
        {
            return Err(denied("existence identity is retired by the signed base"));
        }
        let at = |sequence: u64| {
            rows_with(
                &self.conn,
                "SELECT hash FROM secure_existence_log WHERE scope=? AND signer=? AND sequence=?",
                vec![
                    scope.clone(),
                    signer.clone(),
                    SqlValue::Integer(sequence as i64),
                ],
            )
        };
        if let Some(found) = at(statement.sequence).await?.first() {
            return if found == &[SqlValue::Blob(hash.to_vec())] {
                Ok(ReceiveStatus::Duplicate)
            } else {
                Err(denied("conflicting existence record at one log position"))
            };
        }
        let predecessor = match statement.sequence {
            1 => None,
            sequence => match at(sequence - 1).await?.first().map(Vec::as_slice) {
                None if covered.is_some_and(|log| log.sequence == sequence - 1) => {
                    covered.map(|log| log.head)
                }
                None => return Ok(ReceiveStatus::PendingDependencies),
                Some([SqlValue::Blob(hash)]) => {
                    Some(Commitment::try_from(hash.as_slice()).map_err(|_| corrupt())?)
                }
                Some(_) => return Err(corrupt()),
            },
        };
        if predecessor != statement.previous {
            return Err(denied("existence record names another predecessor"));
        }
        let id = SqlValue::Integer(statement.id.integer());
        let held = rows_with(
            &self.conn,
            "SELECT created,deleted FROM secure_existence WHERE scope=? AND id=?",
            vec![scope.clone(), id.clone()],
        )
        .await?;
        let (created, deleted) = match held.first().map(Vec::as_slice) {
            None => (SqlValue::Null, SqlValue::Null),
            Some([created, deleted]) => (created.clone(), deleted.clone()),
            Some(_) => return Err(corrupt()),
        };
        let retired = held.is_empty()
            && base.as_ref().is_some_and(|base| {
                base.descriptor().is_ok_and(|descriptor| {
                    descriptor.identities.iter().any(|mark| {
                        mark.author == statement.id.author()
                            && mark.sequence >= statement.id.sequence()
                    })
                })
            });
        let mark = SqlValue::Blob(hash.to_vec());
        let (created, deleted) = match statement.event {
            ExistenceEvent::Created if created != SqlValue::Null => {
                return Err(denied("an id is created once"));
            }
            ExistenceEvent::Created => (mark, deleted),
            // A canonical witness also converges for delete-before-create records.
            ExistenceEvent::Deleted
                if deleted == SqlValue::Null
                    || matches!((&deleted,&mark), (SqlValue::Blob(old),SqlValue::Blob(new)) if new < old) =>
            {
                (created, mark)
            }
            ExistenceEvent::Deleted => (created, deleted),
        };
        if !retired {
            self.conn
            .execute(
                "INSERT INTO secure_existence VALUES(?,?,?,?) ON CONFLICT(scope,id) DO UPDATE SET created=excluded.created,deleted=excluded.deleted",
                bindings(vec![scope.clone(), id.clone(), created, deleted]),
            )
            .await?;
        }
        self.conn
            .execute(
                "INSERT INTO secure_existence_log VALUES(?,?,?,?,?,?,?,?)",
                bindings(vec![
                    scope,
                    signer,
                    SqlValue::Integer(statement.sequence as i64),
                    SqlValue::Blob(hash.to_vec()),
                    SqlValue::Blob(record.to_bytes()?),
                    SqlValue::Integer(i64::from(statement.author)),
                    SqlValue::Integer(statement.author_sequence as i64),
                    id,
                ]),
            )
            .await?;
        Box::pin(self.seed_existence(statement.scope, statement.id.integer())).await?;
        Ok(ReceiveStatus::Applied)
    }
    pub(super) async fn rebuild_tip_existence(&self, scope: ResourceScope) -> Result<(), Error> {
        let blob = encode(&scope)?;
        let records=collect_rows(self.conn.query("SELECT record FROM secure_existence_log WHERE scope=? ORDER BY signer,sequence",[blob.clone()]).await?).await?;
        self.conn
            .execute("DELETE FROM secure_existence WHERE scope=?", [blob.clone()])
            .await?;
        for row in records {
            self.index_visible_existence(scope, &blob, &row).await?;
        }
        Ok(())
    }
    /// `rebuild_tip_existence` after a switch that left every record a
    /// `frontier` includes visible: only the ids of `touched` rows and of
    /// records beyond the frontier are indexed again.
    pub(super) async fn rebuild_existence_beyond(
        &self,
        scope: ResourceScope,
        frontier: &[rocky_credentials::AuthorMark],
        touched: &BTreeSet<i64>,
    ) -> Result<(), Error> {
        let blob = encode(&scope)?;
        let mut ids = touched.clone();
        let mut author = -1;
        while let Some([SqlValue::Integer(next)]) = rows_with(
            &self.conn,
            "SELECT MIN(author) FROM secure_existence_log WHERE scope=? AND author>?",
            vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(author)],
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            author = *next;
            let included = frontier
                .iter()
                .find(|mark| i64::from(mark.author) == author)
                .map_or(-1, |mark| mark.sequence as i64);
            for row in rows_with(
                &self.conn,
                "SELECT id FROM secure_existence_log WHERE scope=? AND author=? AND position>?",
                vec![
                    SqlValue::Blob(blob.clone()),
                    SqlValue::Integer(author),
                    SqlValue::Integer(included),
                ],
            )
            .await?
            {
                if let [SqlValue::Integer(id)] = row.as_slice() {
                    ids.insert(*id);
                }
            }
        }
        for id in ids {
            self.conn
                .execute(
                    "DELETE FROM secure_existence WHERE scope=? AND id=?",
                    bindings(vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(id)]),
                )
                .await?;
            for row in rows_with(
                &self.conn,
                "SELECT record FROM secure_existence_log WHERE scope=? AND id=? ORDER BY signer,sequence",
                vec![SqlValue::Blob(blob.clone()), SqlValue::Integer(id)],
            )
            .await?
            {
                self.index_visible_existence(scope, &blob, &row).await?;
            }
        }
        Ok(())
    }
    /// Indexes one stored record of `scope`'s log if the followed tip
    /// shows its frame.
    async fn index_visible_existence(
        &self,
        scope: ResourceScope,
        blob: &[u8],
        row: &[SqlValue],
    ) -> Result<(), Error> {
        let [SqlValue::Blob(bytes)] = row else {
            return Err(corrupt());
        };
        let record = ExistenceRecord::from_bytes(bytes)?;
        let statement = &record.statement;
        if let Some(graph) = self.graphs.get(&scope.domain)
            && !self.visible_frame(
                graph,
                statement.tip,
                statement.author,
                statement.author_sequence,
                self.workspace.tip,
            )?
        {
            return Ok(());
        }
        let column = match statement.event {
            ExistenceEvent::Created => "created",
            ExistenceEvent::Deleted => "deleted",
        };
        self.conn.execute(format!("INSERT INTO secure_existence(scope,id,{column}) VALUES(?,?,?) ON CONFLICT(scope,id) DO UPDATE SET {column}=CASE WHEN {column} IS NULL OR excluded.{column}<{column} THEN excluded.{column} ELSE {column} END"),bindings(vec![SqlValue::Blob(blob.to_vec()),SqlValue::Integer(statement.id.integer()),SqlValue::Blob(record.hash()?.to_vec())])).await?;
        Ok(())
    }
    /// Whether the existence log of `scope` holds a creation of `id` and no
    /// deletion. Missing evidence is not alive.
    pub(super) async fn alive(&self, scope: ResourceScope, id: i64) -> Result<bool, Error> {
        Ok(self.existence(scope, id).await? == Some(true))
    }
    /// None without evidence, else whether the row is alive.
    pub(super) async fn existence(
        &self,
        scope: ResourceScope,
        id: i64,
    ) -> Result<Option<bool>, Error> {
        let found = rows_with(
            &self.conn,
            "SELECT created IS NOT NULL,deleted IS NULL FROM secure_existence WHERE scope=? AND id=?",
            vec![SqlValue::Blob(encode(&scope)?), SqlValue::Integer(id)],
        )
        .await?;
        Ok(match found.first().map(Vec::as_slice) {
            None => {
                let Ok(identity) = RowIdentity::from_integer(id) else {
                    return Ok(None);
                };
                self.existence_base(scope)
                    .await?
                    .as_ref()
                    .map(|base| base.descriptor())
                    .transpose()?
                    .and_then(|base| {
                        base.identities.iter().find(|mark| {
                            mark.author == identity.author() && mark.sequence >= identity.sequence()
                        })
                    })
                    .map(|_| false)
            }
            Some([SqlValue::Integer(created), SqlValue::Integer(undeleted)]) => {
                Some(*created != 0 && *undeleted != 0)
            }
            Some(_) => return Err(corrupt()),
        })
    }
    /// Authorizes a peer of the domain: any valid chain with a grant in it.
    fn authorize_domain_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        self.same_tip_peer(peer)?;
        let grants = self.verify_chain(credentials, peer, now()?)?;
        if !self.existence_table(scope)
            || !grants
                .iter()
                .any(|grant| grant.scope.domain == scope.domain)
        {
            return Err(denied("existence log is outside the peer's domain"));
        }
        Ok(())
    }
    /// The highest contiguous position of every signer's log of `scope`.
    pub(crate) async fn existence_frontier(
        &self,
        scope: ResourceScope,
    ) -> Result<BTreeMap<PublicKey, u64>, Error> {
        let mut frontier: BTreeMap<PublicKey, u64> = self
            .existence_base(scope)
            .await?
            .map(|base| {
                base.cut().map(|cut| {
                    cut.logs
                        .iter()
                        .map(|log| (log.signer, log.sequence))
                        .collect()
                })
            })
            .transpose()?
            .unwrap_or_default();
        if let Some(graph) = self.graphs.get(&scope.domain) {
            for row in rows_with(
                &self.conn,
                "SELECT record FROM secure_existence_log WHERE scope=?",
                vec![SqlValue::Blob(encode(&scope)?)],
            )
            .await?
            {
                let [SqlValue::Blob(bytes)] = row.as_slice() else {
                    return Err(corrupt());
                };
                let record = ExistenceRecord::from_bytes(bytes)?;
                let statement = &record.statement;
                if self.visible_frame(
                    graph,
                    statement.tip,
                    statement.author,
                    statement.author_sequence,
                    self.workspace.tip,
                )? {
                    frontier
                        .entry(record.signer)
                        .and_modify(|sequence| *sequence = (*sequence).max(record.sequence()))
                        .or_insert(record.sequence());
                }
            }
            return Ok(frontier);
        }
        for row in rows_with(
            &self.conn,
            "SELECT signer,max(sequence) FROM secure_existence_log WHERE scope=? GROUP BY signer",
            vec![SqlValue::Blob(encode(&scope)?)],
        )
        .await?
        {
            let [SqlValue::Blob(signer), SqlValue::Integer(sequence)] = row.as_slice() else {
                return Err(corrupt());
            };
            let signer: [u8; 32] = signer.as_slice().try_into().map_err(|_| corrupt())?;
            let signer = PublicKey::from_bytes(&signer).map_err(|_| corrupt())?;
            frontier.insert(signer, *sequence as u64);
        }
        Ok(frontier)
    }
    /// Records after `after` that `known` lacks, scanning at most a few
    /// pages so one request stays bounded.
    async fn existence_records(
        &self,
        scope: ResourceScope,
        known: &BTreeMap<PublicKey, u64>,
        mut after: Option<(PublicKey, u64)>,
        limit: usize,
    ) -> Result<ExistencePage, Error> {
        let limit = limit.clamp(1, MAX_EXISTENCE_PAGE);
        let mut records = Vec::new();
        let mut bytes = 1024;
        for _ in 0..16 {
            let page = self.existence_scan(scope, after, limit).await?;
            for record in page.records {
                let position = (record.signer, record.sequence());
                let visible = match self.graphs.get(&scope.domain) {
                    Some(graph) => self.visible_frame(
                        graph,
                        record.statement.tip,
                        record.statement.author,
                        record.statement.author_sequence,
                        self.workspace.tip,
                    )?,
                    None => true,
                };
                if visible
                    && known
                        .get(&record.signer)
                        .is_none_or(|held| record.sequence() > *held)
                {
                    let size = record.to_bytes()?.len();
                    if records.len() == limit || bytes + size > SECURE_MESSAGE_BYTES {
                        return Ok(ExistencePage {
                            records,
                            next: after,
                        });
                    }
                    bytes += size;
                    records.push(record);
                }
                after = Some(position);
            }
            if page.next.is_none() {
                return Ok(ExistencePage {
                    records,
                    next: None,
                });
            }
        }
        Ok(ExistencePage {
            records,
            next: after,
        })
    }
    async fn existence_scan(
        &self,
        scope: ResourceScope,
        after: Option<(PublicKey, u64)>,
        limit: usize,
    ) -> Result<ExistencePage, Error> {
        let (signer, sequence) = match after {
            Some((signer, sequence)) => (signer.as_bytes().to_vec(), sequence as i64),
            None => (Vec::new(), 0),
        };
        let found = rows_with(
            &self.conn,
            "SELECT record FROM secure_existence_log WHERE scope=? AND (signer,sequence)>(?,?) ORDER BY signer,sequence LIMIT ?",
            vec![
                SqlValue::Blob(encode(&scope)?),
                SqlValue::Blob(signer),
                SqlValue::Integer(sequence),
                SqlValue::Integer(limit as i64),
            ],
        )
        .await?;
        let mut records = Vec::with_capacity(found.len());
        for row in found {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(corrupt());
            };
            records.push(ExistenceRecord::from_bytes(bytes)?);
        }
        let next = (records.len() == limit)
            .then(|| records.last().map(|last| (last.signer, last.sequence())))
            .flatten();
        Ok(ExistencePage { records, next })
    }
}

fn corrupt() -> Error {
    Error::Corrupt("existence log".into())
}
async fn rows_with(
    conn: &ScopedConnection,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<Vec<Vec<SqlValue>>, Error> {
    collect_rows(conn.query(sql.to_string(), bindings(params)).await?).await
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Verifies and stores one existence record. Any device of the domain
    /// may, with or without the scope's key; the row projections that
    /// depend on the row settle in the same transaction.
    pub async fn receive_existence(&self, record: ExistenceRecord) -> Result<ReceiveStatus, Error> {
        if let Some(tuple) = &record.statement.partition {
            {
                self.inner
                    .lock(crate::replica::Class::Background)
                    .await
                    .verify_existence(&record, now()?)?;
            }
            self.register_partition(record.scope(), tuple.clone())
                .await?;
        }
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let hash = record.hash()?;
        let known = rows_with(
            &inner.conn,
            "SELECT 1 FROM secure_existence_log WHERE hash=?",
            vec![SqlValue::Blob(hash.to_vec())],
        )
        .await?;
        if !known.is_empty() {
            return Ok(ReceiveStatus::Duplicate);
        }
        inner.verify_existence(&record, now()?)?;
        let status = transaction!(inner, |inner| {
            let status = inner.store_existence(&record).await?;
            if status == ReceiveStatus::Applied {
                Box::pin(inner.resolve(None)).await?;
                inner
                    .conn
                    .execute(
                        "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                        (),
                    )
                    .await?;
            }
            Ok(status)
        })?;
        inner.publish_revision().await?;
        Ok(status)
    }
    /// Receives records in order; a record waiting for its predecessor
    /// stops at that log.
    pub async fn receive_existence_all(
        &self,
        records: Vec<ExistenceRecord>,
    ) -> Result<Vec<ReceiveStatus>, Error> {
        let mut statuses = Vec::with_capacity(records.len());
        for record in records {
            statuses.push(self.receive_existence(record).await?);
        }
        Ok(statuses)
    }
    /// Records of `scope` after `after` for a peer of the domain. Records
    /// leave the device only once the commits that stored them are durable.
    /// `known` is the peer's frontier: records at or below it are skipped.
    pub async fn existence_page(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        known: &BTreeMap<PublicKey, u64>,
        after: Option<(PublicKey, u64)>,
        limit: usize,
    ) -> Result<ExistencePage, Error> {
        let (page, commit, flusher) = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            inner.authorize_domain_peer(peer, credentials, scope)?;
            let page = inner.existence_records(scope, known, after, limit).await?;
            (page, inner.commit, inner.flusher.clone())
        };
        flusher.run(commit).await?;
        Ok(page)
    }
    /// The highest position held of every signer's log of `scope`.
    pub async fn existence_frontier(
        &self,
        scope: ResourceScope,
    ) -> Result<BTreeMap<PublicKey, u64>, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .existence_frontier(scope)
            .await
    }
    /// Fails unless `peer` is a device of the domain of an existence log.
    pub async fn authorize_existence_peer(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
    ) -> Result<(), Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .authorize_domain_peer(peer, credentials, scope)
    }
    /// The existence logs this replica follows: those of tables it holds a
    /// grant on, read or custody, and of tables such tables reference.
    pub async fn existence_scopes(&self) -> Result<Vec<ResourceScope>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let Some(typed) = &inner.workspace.typed else {
            return Ok(Vec::new());
        };
        let grants = inner.local_grants(now()?)?;
        let schema = typed.rows.schema();
        let keyed = |name: &str| {
            typed.declared_by_name(name).is_some_and(|projected| {
                grants.iter().any(|grant| {
                    grant.scope == projected.scope
                        && (grant.rights.contains(&Right::Read)
                            || grant.rights.contains(&Right::Custody))
                })
            })
        };
        let mut scopes = Vec::new();
        for table in schema.existence_tables() {
            if !table.partition_by.is_empty() {
                let id = schema.table_id(table);
                scopes.extend(
                    inner
                        .partitions
                        .keys()
                        .filter(|scope| {
                            scope.table == id
                                && grants.iter().any(|grant| {
                                    grant.scope == **scope
                                        && (grant.rights.contains(&Right::Read)
                                            || grant.rights.contains(&Right::Custody))
                                })
                        })
                        .copied(),
                );
                continue;
            }
            let referenced = schema.tables.iter().any(|child| {
                keyed(&child.sql_name)
                    && child
                        .foreign_keys
                        .iter()
                        .any(|key| key.table.eq_ignore_ascii_case(&table.sql_name))
            });
            if (keyed(&table.sql_name) || referenced)
                && let Some(projected) = typed.declared_by_name(&table.sql_name)
            {
                scopes.push(projected.scope);
            }
        }
        Ok(scopes)
    }
    /// What the existence log of `scope` says about `id`: None without
    /// evidence, else whether the row is alive.
    pub async fn existence(&self, scope: ResourceScope, id: i64) -> Result<Option<bool>, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .existence(scope, id)
            .await
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    /// Runs `sql` under an armed session gate naming every typed table of
    /// `scopes`, bypassing the session's statement check.
    pub(crate) async fn run_armed(
        &self,
        scopes: &BTreeSet<ResourceScope>,
        sql: &str,
    ) -> Result<u64, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let typed = inner
            .workspace
            .typed
            .clone()
            .ok_or_else(|| denied("no typed tables"))?;
        let plan = Arc::new(typed.plan(scopes)?);
        let gate = inner.conn.connection().session()?.gate;
        gate.lock().map_err(|_| corrupt())?.arm(Some(plan));
        let result = inner.conn.execute(sql.to_string(), ()).await;
        gate.lock().map_err(|_| corrupt())?.arm(None);
        Ok(result?)
    }
}
