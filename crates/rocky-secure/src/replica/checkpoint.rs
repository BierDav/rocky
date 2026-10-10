//! Signed checkpoints, pruning and rebootstrap of one encryption scope.
//! Format and rules: `docs/runtime/encrypted-checkpoints.md`.
mod audit;
mod create;
mod prune;
mod transfer;
use super::*;
pub use audit::AuditReport;
use p2panda_core::Signature;
pub use prune::{PruneBlock, PruneReport};
pub use transfer::{CheckpointPage, HorizonPage, RebootstrapReport};

pub(crate) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_horizon_live_creations(scope INTEGER NOT NULL,hash BLOB NOT NULL,PRIMARY KEY(scope,hash)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_stubs(scope INTEGER NOT NULL,hash BLOB NOT NULL,clock INTEGER NOT NULL,PRIMARY KEY(scope,hash)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_build_stubs(scope INTEGER NOT NULL,hash BLOB NOT NULL,clock INTEGER NOT NULL,PRIMARY KEY(scope,hash)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_identity_marks(scope BLOB NOT NULL,author INTEGER NOT NULL,signer BLOB NOT NULL,sequence INTEGER NOT NULL,PRIMARY KEY(scope,author)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_scope_state(id INTEGER PRIMARY KEY,scope BLOB NOT NULL UNIQUE,rows INTEGER NOT NULL,tombstones INTEGER NOT NULL,digest BLOB NOT NULL,applied INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_retained(id INTEGER PRIMARY KEY CHECK(id=1),records INTEGER NOT NULL,bytes INTEGER NOT NULL,handles INTEGER NOT NULL,handle_digest BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_applied(position INTEGER PRIMARY KEY AUTOINCREMENT,scope INTEGER NOT NULL,hash BLOB NOT NULL UNIQUE)",
    "CREATE INDEX IF NOT EXISTS secure_applied_scope ON secure_applied(scope,position)",
    "CREATE TABLE IF NOT EXISTS secure_logs(scope INTEGER NOT NULL,author BLOB NOT NULL,sequence INTEGER NOT NULL,hash BLOB NOT NULL,clock INTEGER NOT NULL,horizon INTEGER NOT NULL,PRIMARY KEY(scope,author)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_stubs(hash BLOB PRIMARY KEY,scope INTEGER NOT NULL,clock INTEGER NOT NULL,creation INTEGER NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_checkpoints(scope INTEGER NOT NULL,sequence INTEGER NOT NULL,state TEXT NOT NULL,position INTEGER NOT NULL,cursor INTEGER NOT NULL,sets BLOB NOT NULL,capture BLOB,signed BLOB,PRIMARY KEY(scope,sequence))",
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_rows(scope INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_overlay(scope INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_scratch(scope INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_retention(scope BLOB PRIMARY KEY,policy BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_checkpoint_strong(scope INTEGER PRIMARY KEY,state BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_rebootstrap_strong(scope INTEGER PRIMARY KEY,state BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_rebootstrap(scope INTEGER PRIMARY KEY,signed BLOB NOT NULL,cursor BLOB,complete INTEGER NOT NULL,sets BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_rebootstrap_rows(scope INTEGER NOT NULL,row BLOB NOT NULL,fields BLOB NOT NULL,PRIMARY KEY(scope,row)) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_rebootstrap_stubs(scope INTEGER NOT NULL,hash BLOB NOT NULL,clock INTEGER NOT NULL,PRIMARY KEY(scope,hash)) WITHOUT ROWID",
    "CREATE INDEX IF NOT EXISTS secure_handles_hash ON secure_handles(hash)",
    "CREATE INDEX IF NOT EXISTS secure_ack_hash ON secure_ack(hash)",
];

/// Applied operations replayed, or pruned, per bounded step.
#[cfg(not(test))]
pub(super) const STEP_OPERATIONS: usize = 64;
/// Checkpoint rows installed, staged or streamed per bounded step.
#[cfg(not(test))]
pub(super) const STEP_ROWS: usize = 256;
// Small steps let tests cross every step boundary with little data.
#[cfg(test)]
pub(super) const STEP_OPERATIONS: usize = 8;
#[cfg(test)]
pub(super) const STEP_ROWS: usize = 16;
/// Logs of one scope a checkpoint frontier may name.
pub const MAX_CHECKPOINT_LOGS: usize = 256;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct IdentityMark {
    pub author: u32,
    pub signer: PublicKey,
    pub sequence: u32,
}

/// The applied tip of one author's log in a scope.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogPosition {
    pub author: PublicKey,
    pub sequence: u64,
    pub hash: Hash,
    pub clock: u64,
}
/// A set bound by its size and an order-independent digest.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct SetSummary {
    pub count: u64,
    pub digest: Commitment,
}
/// Strong base and suffix artifacts of the scope's domain.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StrongSummary {
    /// Sequence and head at which verification of the retained suffix starts.
    pub base: Option<(u64, Commitment)>,
    /// Applied strong sequence and head of this scope's command rows.
    pub head: Option<(u64, Commitment)>,
    pub rows: Commitment,
    pub results: SetSummary,
    pub commands: SetSummary,
    pub preparations: SetSummary,
    pub observations: SetSummary,
}
/// Recovery copies of the covered operations when the checkpoint was sealed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CustodySummary {
    /// Retained covered operations another node acknowledged holding.
    pub superseded: u64,
    /// Retained covered operations nobody acknowledged: sole copies.
    pub sole: SetSummary,
    /// Recovery archives held for other nodes; a checkpoint never releases them.
    pub archives: SetSummary,
}
/// The current checkpoint statement format; every other one is refused.
pub(crate) const CHECKPOINT_FORMAT: u32 = 7;
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointStatement {
    pub format: u32,
    /// Most recently admitted issuer declaration.
    pub horizon: Option<Box<rocky_credentials::SignedHorizon>>,
    /// Last cut atomically finalized into the replay base.
    pub finalized: Option<Box<rocky_credentials::SignedHorizon>>,
    pub partition: Option<rocky_core::PartitionTuple>,
    pub identities: Vec<IdentityMark>,
    /// Opaque move links, also committed by the corresponding materialized rows.
    pub moves: SetSummary,
    pub definition: Commitment,
    pub schema: u32,
    pub handler: u32,
    /// The digest of the revocations the signer held.
    pub revocations: Commitment,
    pub scope: ResourceScope,
    pub key_epoch: u64,
    /// Length of the checkpoint chain of this scope, starting at 1.
    pub sequence: u64,
    pub previous: Option<Commitment>,
    /// Applied tip of every log, ordered by author.
    pub frontier: Vec<LogPosition>,
    pub clock: u64,
    pub operations: u64,
    /// Stored rows, tombstones and creation identities included.
    pub rows: SetSummary,
    pub tombstones: u64,
    /// Creation operations of the stored rows with their clocks.
    pub creations: SetSummary,
    /// Mutation handles the signer retains.
    pub handles: SetSummary,
    pub strong: StrongSummary,
    pub custody: CustodySummary,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SignedCheckpoint {
    pub statement: CheckpointStatement,
    node: PublicKey,
    credentials: CredentialChain,
    signature: Signature,
}
impl SignedCheckpoint {
    fn signing_bytes(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.encrypted-checkpoint.v3",
            &(&self.statement, self.node, &self.credentials),
        )
    }
    pub(crate) fn sign(
        statement: CheckpointStatement,
        node: &PrivateKey,
        credentials: CredentialChain,
    ) -> Result<Self, Error> {
        let mut signed = Self {
            statement,
            node: node.public_key(),
            credentials,
            signature: node.sign(b""),
        };
        signed.signature = node.sign(&signed.signing_bytes()?);
        signed.to_bytes()?;
        Ok(signed)
    }
    pub fn signer(&self) -> PublicKey {
        self.node
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        let bytes = encode(self)?;
        if bytes.len() > MAX_SECURE_FRAME {
            return Err(Error::Quota {
                resource: "encrypted checkpoint statement",
                limit: MAX_SECURE_FRAME,
            });
        }
        Ok(bytes)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        #[derive(Deserialize)]
        struct Version {
            statement: Format,
        }
        let version: Version =
            p2panda_core::cbor::decode_cbor(bytes).map_err(|_| denied("invalid checkpoint"))?;
        if version.statement.format != CHECKPOINT_FORMAT {
            return Err(denied("unsupported checkpoint format"));
        }
        decode(bytes)
    }
    pub fn id(&self) -> Result<Commitment, Error> {
        Ok(rocky_core::commitment(
            "rocky.encrypted-checkpoint.id.v3",
            &self.to_bytes()?,
        ))
    }
    /// The signature and the frontier's canonical order.
    pub(crate) fn verify_signature(&self) -> Result<(), Error> {
        let statement = &self.statement;
        if statement.format != CHECKPOINT_FORMAT {
            return Err(denied("unsupported checkpoint format"));
        }
        if statement.partition.as_ref().is_some_and(|tuple| {
            tuple
                .scope(statement.scope.domain, statement.scope.table)
                .ok()
                != Some(statement.scope)
        }) || statement.identities.len() > MAX_CHECKPOINT_LOGS
            || statement
                .identities
                .windows(2)
                .any(|pair| pair[0].author >= pair[1].author)
            || statement
                .identities
                .iter()
                .any(|mark| rocky_core::RowIdentity::new(mark.author, mark.sequence).is_err())
            || statement.sequence == 0
            || statement.frontier.len() > MAX_CHECKPOINT_LOGS
            || statement
                .frontier
                .windows(2)
                .any(|pair| pair[0].author >= pair[1].author)
            || !self.node.verify(&self.signing_bytes()?, &self.signature)
        {
            return Err(denied("invalid checkpoint signature or frontier"));
        }
        Ok(())
    }
    pub(crate) fn verify_history_authority<A: RowApplication>(
        &self,
        inner: &Inner<A>,
    ) -> Result<(), Error> {
        self.verify_signature()?;
        inner.verify_checkpoint_horizon(self)?;
        inner.admits(self.statement.definition)?;
        // History verifies with no revocation, at the time it was signed.
        let revocations = RevocationSet::new();
        let time = self
            .credentials
            .0
            .iter()
            .map(|credential| credential.capability.not_before)
            .max()
            .ok_or_else(|| denied("checkpoint credentials missing"))?;
        inner.check_authenticated_partition(
            self.statement.scope,
            self.statement.partition.as_ref(),
        )?;
        let grants = match &self.statement.partition {
            Some(tuple) => self
                .credentials
                .verify_partition(
                    inner.config.publisher,
                    self.statement.definition,
                    self.node,
                    self.statement.scope,
                    tuple,
                    &revocations,
                    time,
                )
                .map_err(credential)?,
            None => self
                .credentials
                .verify(
                    inner.config.publisher,
                    self.statement.definition,
                    self.node,
                    &revocations,
                    time,
                )
                .map_err(credential)?,
        };
        if !grants.iter().any(|grant| {
            grant.scope == self.statement.scope
                && grant.rows == RowSelection::All
                && grant.rights.contains(&Right::Read)
                && [Right::Insert, Right::Update, Right::Delete]
                    .iter()
                    .any(|right| grant.rights.contains(right))
        }) {
            return Err(denied(
                "historical strong checkpoint signer lacks scope write authority",
            ));
        }
        Ok(())
    }
    /// Bootstrap state needs a current whole-scope reader with a write grant,
    /// under the held revocations.
    pub(super) fn verify_authority<A: RowApplication>(
        &self,
        inner: &Inner<A>,
        time: u64,
    ) -> Result<(), Error> {
        self.verify_signature()?;
        inner.verify_checkpoint_horizon(self)?;
        if self
            .statement
            .horizon
            .as_ref()
            .is_some_and(|horizon| horizon.statement.issued > time)
        {
            return Err(denied("checkpoint horizon issue time is in the future"));
        }
        inner.admits(self.statement.definition)?;
        if self
            .credentials
            .0
            .first()
            .is_some_and(|root| root.capability.manifest != self.statement.definition)
        {
            return Err(denied(
                "checkpoint signer's credentials name another definition",
            ));
        }
        inner.check_authenticated_partition(
            self.statement.scope,
            self.statement.partition.as_ref(),
        )?;
        let grants = match &self.statement.partition {
            Some(tuple) => self
                .credentials
                .verify_partition(
                    inner.config.publisher,
                    self.statement.definition,
                    self.node,
                    self.statement.scope,
                    tuple,
                    inner.revocations_for(self.statement.definition, &inner.config.revocations)?,
                    time,
                )
                .map_err(credential)?,
            None => inner.verify_chain(&self.credentials, self.node, time)?,
        };
        if !grants.iter().any(|grant| {
            grant.scope == self.statement.scope
                && grant.rows == RowSelection::All
                && grant.rights.contains(&Right::Read)
                && [Right::Insert, Right::Update, Right::Delete]
                    .iter()
                    .any(|right| grant.rights.contains(right))
        }) {
            return Err(denied(
                "checkpoint signer lacks whole-scope read and write authority",
            ));
        }
        Ok(())
    }
}

/// When covered operations of a scope may be pruned. Local configuration.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct RetentionPolicy {
    /// Peers that must have acknowledged an operation before it is pruned.
    pub required: BTreeSet<PublicKey>,
    /// Peers declared behind the horizon: not waited for; they rebootstrap.
    pub behind: BTreeSet<PublicKey>,
    /// Other nodes that must hold a copy; at least one is always required.
    pub copies: usize,
    /// Newest covered operations kept for peers that catch up without a checkpoint.
    pub window: usize,
}
/// Replica-wide bounds on retained operations. Not persisted.
#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct RetentionLimits {
    /// Applied operations after a scope's checkpoint that start the next one.
    pub checkpoint_operations: usize,
    /// Retained header and body bytes that start checkpointing and pruning.
    pub soft_bytes: usize,
    /// Retained bytes at which authoring and receiving report backpressure.
    pub hard_bytes: usize,
    /// Most recent mutation handles kept for retries after their operations
    /// are pruned. An older handle is forgotten; its retry authors again.
    pub handles: usize,
}
impl Default for RetentionLimits {
    fn default() -> Self {
        Self {
            checkpoint_operations: 1024,
            soft_bytes: MAX_SECURE_BYTES,
            hard_bytes: 2 * MAX_SECURE_BYTES,
            handles: 65_536,
        }
    }
}
#[derive(Clone, Debug)]
pub struct CheckpointStatus {
    /// The latest verified checkpoint.
    pub checkpoint: Option<SignedCheckpoint>,
    /// A newer checkpoint is being built or installed.
    pub in_progress: bool,
    /// Applied operations after the verified checkpoint.
    pub uncovered: u64,
    /// Retained operations at or before it.
    pub covered: u64,
    /// Admitted operations of the scope that are not applied.
    pub pending: u64,
    /// Logs with pruned history and the last pruned sequence of each.
    pub horizon: Vec<(PublicKey, u64)>,
    /// Retained header and body bytes of the whole replica.
    pub retained_bytes: u64,
    /// The last failure of automatic maintenance.
    pub maintenance_error: Option<String>,
}

/// Fault points of the bounded maintenance steps.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Stage {
    Begin,
    Replay,
    Seal,
    Install,
    Prune,
    Import,
    Switch,
    Evict,
}

const LANES: usize = 1024;
/// LtHash16: an order-independent digest that elements can be added to and
/// removed from. Each element expands to 1,024 lanes added modulo 2^16.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Lt(Vec<u16>);
impl Lt {
    pub(super) fn new() -> Self {
        Self(vec![0; LANES])
    }
    pub(super) fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 2 * LANES {
            return Err(Error::Corrupt("set digest state".into()));
        }
        Ok(Self(
            bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
                .collect(),
        ))
    }
    pub(super) fn to_bytes(&self) -> Vec<u8> {
        self.0.iter().flat_map(|lane| lane.to_le_bytes()).collect()
    }
    fn mix(&mut self, label: &str, item: &[u8], add: bool) {
        let mut seed = [0u8; 33];
        seed[..32].copy_from_slice(&rocky_core::commitment(label, item));
        for block in 0..LANES / 16 {
            seed[32] = block as u8;
            let lanes = rocky_core::commitment("rocky.checkpoint-lanes.v1", &seed);
            for (index, pair) in lanes.as_chunks::<2>().0.iter().enumerate() {
                let value = u16::from_le_bytes([pair[0], pair[1]]);
                let lane = &mut self.0[block * 16 + index];
                *lane = if add {
                    lane.wrapping_add(value)
                } else {
                    lane.wrapping_sub(value)
                };
            }
        }
    }
    pub(super) fn add(&mut self, label: &str, item: &[u8]) {
        self.mix(label, item, true);
    }
    pub(super) fn remove(&mut self, label: &str, item: &[u8]) {
        self.mix(label, item, false);
    }
    pub(super) fn digest(&self) -> Commitment {
        rocky_core::commitment("rocky.checkpoint-set.v1", &self.to_bytes())
    }
}
pub(super) const ROW_LABEL: &str = "rocky.checkpoint-row.v2";
pub(super) const CREATION_LABEL: &str = "rocky.checkpoint-creation.v1";
pub(super) const HANDLE_LABEL: &str = "rocky.checkpoint-handle.v1";

/// Stored rows of a scope and the creation operations behind them.
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Sets {
    pub rows: u64,
    pub tombstones: u64,
    pub digest: Lt,
    pub creations: u64,
    pub creation_digest: Lt,
}
impl Sets {
    pub(super) fn new() -> Self {
        Self {
            rows: 0,
            tombstones: 0,
            digest: Lt::new(),
            creations: 0,
            creation_digest: Lt::new(),
        }
    }
    pub(super) fn to_bytes(&self) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(24 + 4 * LANES);
        for count in [self.rows, self.tombstones, self.creations] {
            bytes.extend(count.to_le_bytes());
        }
        bytes.extend(self.digest.to_bytes());
        bytes.extend(self.creation_digest.to_bytes());
        bytes
    }
    pub(super) fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != 24 + 4 * LANES {
            return Err(Error::Corrupt("checkpoint set state".into()));
        }
        let count = |index: usize| {
            let mut value = [0; 8];
            value.copy_from_slice(&bytes[index * 8..index * 8 + 8]);
            u64::from_le_bytes(value)
        };
        Ok(Self {
            rows: count(0),
            tombstones: count(1),
            creations: count(2),
            digest: Lt::from_bytes(&bytes[24..24 + 2 * LANES])?,
            creation_digest: Lt::from_bytes(&bytes[24 + 2 * LANES..])?,
        })
    }
    /// Replaces one stored row record by another.
    pub(super) fn replace(
        &mut self,
        row: &Commitment,
        old: Option<&[u8]>,
        new: &[u8],
    ) -> Result<(), Error> {
        if let Some(old) = old {
            self.digest
                .remove(ROW_LABEL, &[row.as_slice(), old].concat());
            self.rows -= 1;
            self.tombstones -= u64::from(decode::<Materialized>(old)?.deleted);
        }
        self.digest.add(ROW_LABEL, &[row.as_slice(), new].concat());
        self.rows += 1;
        self.tombstones += u64::from(decode::<Materialized>(new)?.deleted);
        Ok(())
    }
    pub(super) fn create(&mut self, hash: Hash, clock: u64) {
        self.creations += 1;
        self.creation_digest
            .add(CREATION_LABEL, &creation_item(hash, clock));
    }
    pub(super) fn rows_summary(&self) -> SetSummary {
        SetSummary {
            count: self.rows,
            digest: self.digest.digest(),
        }
    }
    pub(super) fn creations_summary(&self) -> SetSummary {
        SetSummary {
            count: self.creations,
            digest: self.creation_digest.digest(),
        }
    }
}
pub(super) fn creation_item(hash: Hash, clock: u64) -> Vec<u8> {
    [hash.as_bytes().as_slice(), &clock.to_le_bytes()].concat()
}
pub(super) fn handle_item(handle: &[u8], input: &[u8], hash: &[u8]) -> Vec<u8> {
    [handle, input, hash].concat()
}

/// What a checkpoint must reproduce, captured in one transaction.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Capture {
    pub horizon: Option<Box<rocky_credentials::SignedHorizon>>,
    /// Last cut atomically finalized into the replay base.
    pub finalized: Option<Box<rocky_credentials::SignedHorizon>>,
    pub identities: Vec<IdentityMark>,
    pub frontier: Vec<LogPosition>,
    pub clock: u64,
    pub operations: u64,
    pub rows: SetSummary,
    pub tombstones: u64,
    pub handles: SetSummary,
    pub previous: Option<Commitment>,
}
pub(super) struct Record {
    pub sequence: i64,
    pub state: String,
    pub position: i64,
    pub cursor: i64,
    pub sets: Sets,
    pub capture: Option<Capture>,
    pub signed: Option<SignedCheckpoint>,
}
/// One author's log in a scope: its applied tip and pruned prefix.
pub(super) struct Log {
    pub sequence: i64,
    pub hash: Hash,
    pub clock: u64,
    pub horizon: i64,
}
/// Which replay overlay a caller uses over the checkpoint rows.
#[derive(Clone, Copy)]
pub(super) enum Overlay {
    Build,
    Scratch,
}
impl Overlay {
    pub(super) fn table(self) -> &'static str {
        match self {
            Self::Build => "secure_checkpoint_overlay",
            Self::Scratch => "secure_checkpoint_scratch",
        }
    }
}

pub(super) fn hash_of(bytes: &[u8]) -> Result<Hash, Error> {
    Hash::try_from(bytes).map_err(|_| Error::Corrupt("stored operation hash".into()))
}
pub(super) fn key_of(bytes: &[u8]) -> Result<PublicKey, Error> {
    PublicKey::try_from(bytes).map_err(|_| Error::Corrupt("stored author key".into()))
}
pub(super) fn row_of(bytes: &[u8]) -> Result<Commitment, Error> {
    bytes
        .try_into()
        .map_err(|_| Error::Corrupt("stored row identity".into()))
}
fn integer(value: u64) -> SqlValue {
    SqlValue::Integer(value as i64)
}

impl<A: RowApplication> Inner<A> {
    #[cfg(test)]
    pub(crate) fn fault(&mut self, stage: Stage) -> Result<(), Error> {
        if let Some((at, skip)) = &mut self.stage_fault
            && *at == stage
        {
            if *skip == 0 {
                if self.stage_exit {
                    std::process::exit(77);
                }
                self.stage_fault = None;
                return Err(Error::InjectedFailure);
            }
            *skip -= 1;
        }
        Ok(())
    }
    #[cfg(not(test))]
    pub(crate) fn fault(&mut self, _stage: Stage) -> Result<(), Error> {
        Ok(())
    }
    pub(super) async fn find_scope_id(&self, scope: ResourceScope) -> Result<Option<i64>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT id FROM secure_scope_state WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        Ok(match result.next().await? {
            Some(row) => Some(row.get::<i64>(0)?),
            None => None,
        })
    }
    /// The scope's number, assigned inside the caller's write transaction.
    pub(super) async fn scope_id(&self, scope: ResourceScope) -> Result<i64, Error> {
        if let Some(id) = self.find_scope_id(scope).await? {
            return Ok(id);
        }
        self.conn
            .execute(
                "INSERT INTO secure_scope_state(scope,rows,tombstones,digest,applied) VALUES(?,0,0,?,0)",
                [encode(&scope)?, Lt::new().to_bytes()],
            )
            .await?;
        self.find_scope_id(scope)
            .await?
            .ok_or_else(|| Error::Corrupt("scope state".into()))
    }
    /// Live rows, tombstones and digest of a scope, and its applied operations.
    pub(super) async fn scope_state(&self, id: i64) -> Result<(u64, u64, Lt, u64), Error> {
        let mut result = self
            .conn
            .query(
                "SELECT rows,tombstones,digest,applied FROM secure_scope_state WHERE id=?",
                [id],
            )
            .await?;
        let row = result
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("scope state".into()))?;
        Ok((
            row.get::<i64>(0)? as u64,
            row.get::<i64>(1)? as u64,
            Lt::from_bytes(&row.get::<Vec<u8>>(2)?)?,
            row.get::<i64>(3)? as u64,
        ))
    }
    pub(super) async fn store_scope_state(
        &self,
        id: i64,
        rows: u64,
        tombstones: u64,
        digest: &Lt,
        applied: u64,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "UPDATE secure_scope_state SET rows=?,tombstones=?,digest=?,applied=? WHERE id=?",
                bindings(vec![
                    integer(rows),
                    integer(tombstones),
                    SqlValue::Blob(digest.to_bytes()),
                    integer(applied),
                    SqlValue::Integer(id),
                ]),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn log(&self, scope: i64, author: PublicKey) -> Result<Option<Log>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT sequence,hash,clock,horizon FROM secure_logs WHERE scope=? AND author=?",
                bindings(vec![
                    SqlValue::Integer(scope),
                    SqlValue::Blob(author.as_bytes().to_vec()),
                ]),
            )
            .await?;
        let Some(row) = result.next().await? else {
            return Ok(None);
        };
        Ok(Some(Log {
            sequence: row.get::<i64>(0)?,
            hash: hash_of(&row.get::<Vec<u8>>(1)?)?,
            clock: row.get::<i64>(2)? as u64,
            horizon: row.get::<i64>(3)?,
        }))
    }
    /// Every log of a scope in author order; `pruned` keeps only logs with a horizon.
    pub(super) async fn logs(
        &self,
        scope: i64,
        after: Option<PublicKey>,
        pruned: bool,
        limit: usize,
    ) -> Result<Vec<(LogPosition, i64)>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT author,sequence,hash,clock,horizon FROM secure_logs WHERE scope=? AND author>? AND horizon>=? ORDER BY author LIMIT ?",
                    bindings(vec![
                        SqlValue::Integer(scope),
                        SqlValue::Blob(after.map(|key| key.as_bytes().to_vec()).unwrap_or_default()),
                        SqlValue::Integer(if pruned { 0 } else { -1 }),
                        SqlValue::Integer(limit as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        records
            .into_iter()
            .map(|record| {
                let [
                    SqlValue::Blob(author),
                    SqlValue::Integer(sequence),
                    SqlValue::Blob(hash),
                    SqlValue::Integer(clock),
                    SqlValue::Integer(horizon),
                ] = record.as_slice()
                else {
                    return Err(Error::Corrupt("log record".into()));
                };
                Ok((
                    LogPosition {
                        author: key_of(author)?,
                        sequence: *sequence as u64,
                        hash: hash_of(hash)?,
                        clock: *clock as u64,
                    },
                    *horizon,
                ))
            })
            .collect()
    }
    pub(super) async fn retains(&self, hash: Hash) -> Result<bool, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT 1 FROM secure_operations WHERE hash=?",
                [hash.as_bytes().to_vec()],
            )
            .await?;
        Ok(result.next().await?.is_some())
    }
    /// A pruned operation still known as applied: its scope and clock.
    pub(super) async fn stub(&self, hash: Hash) -> Result<Option<(i64, u64)>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT scope,clock FROM secure_stubs WHERE hash=?",
                [hash.as_bytes().to_vec()],
            )
            .await?;
        Ok(match result.next().await? {
            Some(row) => Some((row.get::<i64>(0)?, row.get::<i64>(1)? as u64)),
            None => None,
        })
    }
    pub(super) async fn put_stub(
        &self,
        hash: Hash,
        scope: i64,
        clock: u64,
        creation: bool,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO secure_stubs VALUES(?,?,?,?) ON CONFLICT(hash) DO UPDATE SET creation=MAX(creation,excluded.creation)",
                bindings(vec![
                    SqlValue::Blob(hash.as_bytes().to_vec()),
                    SqlValue::Integer(scope),
                    integer(clock),
                    SqlValue::Integer(i64::from(creation)),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Records an applied operation: its position and its log's new tip.
    pub(super) async fn note_applied(
        &self,
        scope: i64,
        operation: &p2panda_core::Operation<Extensions>,
    ) -> Result<(), Error> {
        self.conn
            .execute(
                "INSERT INTO secure_applied(scope,hash) VALUES(?,?)",
                bindings(vec![
                    SqlValue::Integer(scope),
                    SqlValue::Blob(operation.hash.as_bytes().to_vec()),
                ]),
            )
            .await?;
        self.conn
            .execute(
                "INSERT INTO secure_logs VALUES(?,?,?,?,?,-1) ON CONFLICT(scope,author) DO UPDATE SET sequence=excluded.sequence,hash=excluded.hash,clock=excluded.clock",
                bindings(vec![
                    SqlValue::Integer(scope),
                    SqlValue::Blob(operation.header.public_key.as_bytes().to_vec()),
                    integer(operation.header.seq_num),
                    SqlValue::Blob(operation.hash.as_bytes().to_vec()),
                    integer(operation.header.timestamp),
                ]),
            )
            .await?;
        Ok(())
    }
    /// Retained operations and their header and body bytes.
    pub(super) async fn retained(&self) -> Result<(u64, u64), Error> {
        let record = rows(&self.conn, "SELECT records,bytes FROM secure_retained").await?;
        match record.first().map(Vec::as_slice) {
            Some([SqlValue::Integer(records), SqlValue::Integer(bytes)]) => {
                Ok((*records as u64, *bytes as u64))
            }
            _ => Err(Error::Corrupt("retained history counters".into())),
        }
    }
    pub(super) async fn handle_set(&self) -> Result<(u64, Lt), Error> {
        let mut result = self
            .conn
            .query("SELECT handles,handle_digest FROM secure_retained", ())
            .await?;
        let row = result
            .next()
            .await?
            .ok_or_else(|| Error::Corrupt("retained history counters".into()))?;
        Ok((
            row.get::<i64>(0)? as u64,
            Lt::from_bytes(&row.get::<Vec<u8>>(1)?)?,
        ))
    }
    /// Stores a mutation handle and adds it to the retained handle set.
    pub(crate) async fn put_handle(
        &self,
        handle: &[u8],
        input: &[u8],
        hash: &[u8],
    ) -> Result<(), Error> {
        let inserted = self
            .conn
            .execute(
                "INSERT INTO secure_handles VALUES(?,?,?) ON CONFLICT(handle) DO NOTHING",
                [handle.to_vec(), input.to_vec(), hash.to_vec()],
            )
            .await?;
        if inserted == 1 {
            let (count, mut digest) = self.handle_set().await?;
            digest.add(HANDLE_LABEL, &handle_item(handle, input, hash));
            self.conn
                .execute(
                    "UPDATE secure_retained SET handles=?,handle_digest=?",
                    bindings(vec![integer(count + 1), SqlValue::Blob(digest.to_bytes())]),
                )
                .await?;
        }
        Ok(())
    }
    /// Forgets up to `limit` of the oldest handles beyond `limits.handles`
    /// whose operations are pruned. Handles of retained operations stay.
    pub(super) async fn forget_handles(&self, limit: usize) -> Result<(), Error> {
        let (count, mut digest) = self.handle_set().await?;
        let excess = count.saturating_sub(self.limits.handles as u64);
        if excess == 0 {
            return Ok(());
        }
        let forgotten = collect_rows(
            self.conn
                .query(
                    "SELECT rowid,handle,input,hash FROM secure_handles h WHERE NOT EXISTS(SELECT 1 FROM secure_operations o WHERE o.hash=h.hash) ORDER BY rowid LIMIT ?",
                    [excess.min(limit as u64) as i64],
                )
                .await?,
        )
        .await?;
        for record in &forgotten {
            let [
                SqlValue::Integer(rowid),
                SqlValue::Blob(handle),
                SqlValue::Blob(input),
                SqlValue::Blob(hash),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("mutation handle".into()));
            };
            self.conn
                .execute("DELETE FROM secure_handles WHERE rowid=?", [*rowid])
                .await?;
            self.conn
                .execute(
                    "DELETE FROM secure_bundle_handles WHERE handle=?",
                    [handle.clone()],
                )
                .await?;
            digest.remove(HANDLE_LABEL, &handle_item(handle, input, hash));
        }
        if !forgotten.is_empty() {
            self.conn
                .execute(
                    "UPDATE secure_retained SET handles=?,handle_digest=?",
                    bindings(vec![
                        integer(count - forgotten.len() as u64),
                        SqlValue::Blob(digest.to_bytes()),
                    ]),
                )
                .await?;
        }
        Ok(())
    }
    /// The newest checkpoint record of a scope, or its newest verified one.
    pub(super) async fn checkpoint_record(
        &self,
        scope: i64,
        verified: bool,
    ) -> Result<Option<Record>, Error> {
        let sql = if verified {
            "SELECT sequence,state,position,cursor,sets,capture,signed FROM secure_checkpoints WHERE scope=? AND state='verified' ORDER BY sequence DESC LIMIT 1"
        } else {
            "SELECT sequence,state,position,cursor,sets,capture,signed FROM secure_checkpoints WHERE scope=? ORDER BY sequence DESC LIMIT 1"
        };
        let mut result = self.conn.query(sql, [scope]).await?;
        let Some(row) = result.next().await? else {
            return Ok(None);
        };
        let blob = |index| -> Result<Option<Vec<u8>>, Error> {
            Ok(match row.get_value(index)? {
                crate::sqlite::Value::Blob(bytes) => Some(bytes),
                _ => None,
            })
        };
        Ok(Some(Record {
            sequence: row.get::<i64>(0)?,
            state: row.get::<String>(1)?,
            position: row.get::<i64>(2)?,
            cursor: row.get::<i64>(3)?,
            sets: Sets::from_bytes(&row.get::<Vec<u8>>(4)?)?,
            capture: blob(5)?.map(|bytes| decode(&bytes)).transpose()?,
            signed: blob(6)?
                .map(|bytes| SignedCheckpoint::from_bytes(&bytes))
                .transpose()?,
        }))
    }
    /// A stored checkpoint is used only while it still verifies: signature,
    /// installed definition, scope and the row sets it was sealed over.
    pub(super) fn verify_record(
        &self,
        scope: ResourceScope,
        record: &Record,
    ) -> Result<SignedCheckpoint, Error> {
        let unverifiable = |reason: &str| Error::Corrupt(format!("checkpoint {reason}"));
        let signed = record
            .signed
            .clone()
            .ok_or_else(|| unverifiable("statement is missing"))?;
        signed
            .verify_signature()
            .map_err(|_| unverifiable("signature is invalid"))?;
        let statement = &signed.statement;
        // A checkpoint made before an upgrade stays valid while its
        // generation is admitted.
        let generation = self
            .generation_of(statement.definition, false)
            .map_err(|_| unverifiable("names a definition this replica no longer admits"))?;
        if statement.schema != generation.schema
            || statement.handler != generation.handler
            || statement.scope != scope
            || statement.rows != record.sets.rows_summary()
            || statement.tombstones != record.sets.tombstones
            || statement.creations != record.sets.creations_summary()
        {
            return Err(unverifiable(
                "differs from the installed definition or stored rows",
            ));
        }
        Ok(signed)
    }
    /// The key of a scope this node may read in whole.
    pub(super) fn scope_key(&self, scope: ResourceScope) -> Result<(u64, [u8; 32]), Error> {
        if !self.can_read_key(scope, &self.local_grants(now()?)?) {
            return Err(denied("checkpoint needs whole-scope read authority"));
        }
        self.config
            .keys
            .iter()
            .find(|key| key.scope == scope)
            .map(|key| (key.generation, key.key))
            .ok_or_else(|| denied("checkpoint needs the scope key"))
    }
    /// One stored row record from an overlay, else from the checkpoint rows.
    async fn replay_row(
        &self,
        overlay: Overlay,
        scope: i64,
        row: Commitment,
    ) -> Result<Option<Vec<u8>>, Error> {
        for table in [overlay.table(), "secure_checkpoint_rows"] {
            let mut result = self
                .conn
                .query(
                    format!("SELECT fields FROM {table} WHERE scope=? AND row=?"),
                    bindings(vec![SqlValue::Integer(scope), SqlValue::Blob(row.to_vec())]),
                )
                .await?;
            if let Some(record) = result.next().await? {
                return Ok(Some(record.get::<Vec<u8>>(0)?));
            }
        }
        Ok(None)
    }
    /// Replays applied operations with positions in `(after, through]` in
    /// application order into `overlay`, at most `limit`, and folds every row
    /// change into `sets`. Each operation opens with the key epoch that
    /// sealed it. Returns the last position replayed.
    pub(super) async fn replay(
        &self,
        scope: ResourceScope,
        id: i64,
        overlay: Overlay,
        (after, through): (i64, i64),
        limit: usize,
        sets: &mut Sets,
    ) -> Result<Option<i64>, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT a.position,o.header,o.body FROM secure_applied a LEFT JOIN secure_operations o ON o.hash=a.hash WHERE a.scope=? AND a.position>? AND a.position<=? ORDER BY a.position LIMIT ?",
                    bindings(vec![
                        SqlValue::Integer(id),
                        SqlValue::Integer(after),
                        SqlValue::Integer(through),
                        SqlValue::Integer(limit as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        let mut last = None;
        let basis = self.finalized_basis(scope).await?;
        let finalized = basis.is_some();
        for record in records {
            let [
                SqlValue::Integer(position),
                SqlValue::Blob(header),
                SqlValue::Blob(body),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt(
                    "an applied operation after the checkpoint is not retained".into(),
                ));
            };
            let frame = EncryptedFrame {
                header: header.clone(),
                body: body.clone(),
            };
            let operation = frame.operation()?;
            if self.known(operation.header.extensions.manifest).is_err()
                || !self.dependencies_ready(&frame, &BTreeSet::new()).await?
            {
                return Err(denied("applied encrypted dependency missing"));
            }
            let payload = self.row_payload(&frame)?;
            let mut created = false;
            let key = self
                .opening_key(scope, operation.header.extensions.generation)
                .ok_or_else(|| {
                    denied("an applied operation's key epoch is not installed; it is needed until a checkpoint covers it")
                })?;
            for write in restore::replay_changes(&(scope, key.key), &frame)? {
                if self
                    .app
                    .row_partition(&write.change)
                    .map_err(|error| denied(&error))?
                    != operation.header.extensions.partition
                {
                    return Err(denied("replayed row differs from its partition"));
                }

                let row = self.storage_row(&write.change)?;
                let old = self.replay_row(overlay, id, row).await?;
                let current: Option<Materialized> = old.as_deref().map(decode).transpose()?;
                if finalized
                    && ((write.change.before.is_some() && current.is_none())
                        || (write.change.before.is_none() && current.is_some()))
                {
                    continue;
                }
                if finalized
                    && write.change.before.is_none()
                    && self.protected_conflict(&write.change, &payload).await?
                {
                    continue;
                }
                if write.change.before.is_none()
                    && basis.is_some_and(|(sequence, _)| {
                        payload
                            .basis()
                            .is_none_or(|(authored, _)| authored < sequence)
                    })
                {
                    continue;
                }
                created |= current.is_none();
                let (_, mut record) = apply::merge_row(
                    &write,
                    current,
                    &operation,
                    payload.carried(&write.change.row),
                )?;
                if write.change.before.is_none() && payload.carried(&write.change.row).is_none() {
                    record.proof = payload.proof(&write.change.row).cloned();
                }
                self.release_protection(&mut record, &write, &operation)?;
                record.key = Some(write.change.row);
                record.identity = self
                    .app
                    .row_identity(&write.change)
                    .map_err(|message| denied(&message))?
                    .map(|identity| identity.integer());
                let new = encode(&record)?;
                sets.replace(&row, old.as_deref(), &new)?;
                self.conn
                    .execute(
                        format!("INSERT OR REPLACE INTO {} VALUES(?,?,?)", overlay.table()),
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(row.to_vec()),
                            SqlValue::Blob(new),
                        ]),
                    )
                    .await?;
            }
            if created {
                sets.create(operation.hash, operation.header.timestamp);
                if matches!(overlay, Overlay::Build) {
                    self.put_stub(operation.hash, id, operation.header.timestamp, true)
                        .await?;
                }
            }
            last = Some(*position);
        }
        Ok(last)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Sets when covered operations of a scope may be pruned. Without a
    /// policy nothing is pruned.
    pub async fn set_retention_policy(
        &self,
        scope: ResourceScope,
        policy: RetentionPolicy,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        if policy.required.len() > 64
            || policy.behind.len() > 64
            || policy.copies > 64
            || policy.required.contains(&inner.config.node.public_key())
        {
            return Err(denied("invalid retention policy"));
        }
        if !inner.local_grants(now()?)?.iter().any(|grant| {
            grant.scope == scope
                && grant.rows == RowSelection::All
                && (grant.rights.contains(&Right::Read) || grant.rights.contains(&Right::Custody))
        }) {
            return Err(denied("retention configuration denied"));
        }
        let values = [encode(&scope)?, encode(&policy)?];
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT INTO secure_retention VALUES(?,?) ON CONFLICT(scope) DO UPDATE SET policy=excluded.policy",
                    values.clone(),
                )
                .await?;
            Ok(())
        })
    }
    /// The persisted pruning policy, if one was configured.
    pub async fn retention_policy(
        &self,
        scope: ResourceScope,
    ) -> Result<Option<RetentionPolicy>, Error> {
        self.inner.lock().await.retention_policy(scope).await
    }
    /// The retained-history bounds of this open replica.
    pub async fn retention_limits(&self) -> RetentionLimits {
        self.inner.lock().await.limits
    }
    /// Replaces the retained-history bounds of this open replica.
    pub async fn set_retention_limits(&self, limits: RetentionLimits) -> Result<(), Error> {
        if limits.checkpoint_operations == 0
            || limits.soft_bytes == 0
            || limits.hard_bytes < limits.soft_bytes
        {
            return Err(denied("invalid retention limits"));
        }
        self.inner.lock().await.limits = limits;
        Ok(())
    }
    pub async fn checkpoint_status(&self, scope: ResourceScope) -> Result<CheckpointStatus, Error> {
        let inner = self.inner.lock().await;
        let mut status = CheckpointStatus {
            checkpoint: None,
            in_progress: false,
            uncovered: 0,
            covered: 0,
            pending: 0,
            horizon: vec![],
            retained_bytes: inner.retained().await?.1,
            maintenance_error: inner.maintenance_error.clone(),
        };
        let pending = collect_rows(
            inner
                .conn
                .query(
                    "SELECT COUNT(*) FROM secure_operations WHERE scope=? AND status='pending'",
                    [encode(&scope)?],
                )
                .await?,
        )
        .await?;
        if let Some([SqlValue::Integer(count)]) = pending.first().map(Vec::as_slice) {
            status.pending = *count as u64;
        }
        let Some(id) = inner.find_scope_id(scope).await? else {
            return Ok(status);
        };
        let verified = inner.checkpoint_record(id, true).await?;
        let position = verified.as_ref().map_or(0, |record| record.position);
        status.in_progress = inner
            .checkpoint_record(id, false)
            .await?
            .is_some_and(|record| ["building", "installing"].contains(&record.state.as_str()));
        status.checkpoint = verified.and_then(|record| record.signed);
        let counts = collect_rows(
            inner
                .conn
                .query(
                    "SELECT COALESCE(SUM(position>?),0),COALESCE(SUM(position<=?),0) FROM secure_applied WHERE scope=?",
                    [position, position, id],
                )
                .await?,
        )
        .await?;
        if let Some([SqlValue::Integer(uncovered), SqlValue::Integer(covered)]) =
            counts.first().map(Vec::as_slice)
        {
            status.uncovered = *uncovered as u64;
            status.covered = *covered as u64;
        }
        status.horizon = inner
            .logs(id, None, true, MAX_CHECKPOINT_LOGS)
            .await?
            .into_iter()
            .map(|(log, horizon)| (log.author, horizon as u64))
            .collect();
        Ok(status)
    }
    /// The operation a mutation handle committed, also after it was pruned,
    /// while the handle is inside `RetentionLimits::handles`.
    pub async fn lookup_mutation(&self, handle: Commitment) -> Result<Option<Hash>, Error> {
        let inner = self.inner.lock().await;
        let mut result = inner
            .conn
            .query(
                "SELECT hash FROM secure_handles WHERE handle=?",
                [handle.to_vec()],
            )
            .await?;
        match result.next().await? {
            Some(row) => Ok(Some(hash_of(&row.get::<Vec<u8>>(0)?)?)),
            None => Ok(None),
        }
    }
    #[cfg(test)]
    pub(crate) async fn verify_checkpoint_at_for_test(
        &self,
        signed: &SignedCheckpoint,
        time: u64,
    ) -> Result<(), Error> {
        signed.verify_authority(&*self.inner.lock().await, time)
    }
    #[cfg(test)]
    pub(crate) async fn maintain_checkpoint_for_test(&self, scope: ResourceScope) {
        self.inner.lock().await.maintain(&[scope].into()).await;
    }
    #[cfg(test)]
    pub(crate) async fn inject_stage_fault(&self, stage: Stage, skip: usize) {
        self.inner.lock().await.stage_fault = Some((stage, skip));
    }
    /// Exits the process inside the step's transaction instead of failing it.
    #[cfg(test)]
    pub(crate) async fn inject_stage_exit(&self, stage: Stage, skip: usize) {
        let mut inner = self.inner.lock().await;
        inner.stage_fault = Some((stage, skip));
        inner.stage_exit = true;
    }
}

impl<A: RowApplication> Inner<A> {
    pub(in crate::replica) async fn capture_checkpoint_creations(
        &self,
        scope: ResourceScope,
        id: i64,
        table: &str,
    ) -> Result<(), Error> {
        self.conn
            .execute(format!("DELETE FROM {table} WHERE scope=?"), [id])
            .await?;
        self.conn.execute(format!("INSERT INTO {table} SELECT scope,hash,clock FROM secure_stubs WHERE scope=? AND creation=1"), [id]).await?;
        let mut after = Vec::new();
        loop {
            let rows = collect_rows(self.conn.query("SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?", bindings(vec![SqlValue::Blob(encode(&scope)?),SqlValue::Blob(after.clone()),SqlValue::Integer(STEP_ROWS as i64)])).await?).await?;
            if rows.is_empty() {
                break;
            }
            for row in rows {
                let [SqlValue::Blob(key), SqlValue::Blob(bytes)] = row.as_slice() else {
                    return Err(Error::Corrupt("creation witness row".into()));
                };
                after = key.clone();
                let record: Materialized = decode(bytes)?;
                let clock = match self.stub(record.creation).await? {
                    Some((_, clock)) => clock,
                    None => {
                        self.frame(record.creation)
                            .await?
                            .ok_or_else(|| denied("checkpoint creation witness missing"))?
                            .operation()?
                            .header
                            .timestamp
                    }
                };
                self.conn
                    .execute(
                        format!("INSERT OR IGNORE INTO {table} VALUES(?,?,?)"),
                        bindings(vec![
                            SqlValue::Integer(id),
                            SqlValue::Blob(record.creation.as_bytes().to_vec()),
                            integer(clock),
                        ]),
                    )
                    .await?;
            }
        }
        Ok(())
    }
}
