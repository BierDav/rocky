mod execution;
pub(crate) mod history;
mod projection;
mod ticket;
mod transfer;
use super::replica::Inner;
use super::*;
use crate::storage::{bindings, collect_rows, rows};
use p2panda_core::Signature;
use rocky_core::{
    AuthorSequence, CommandRows, Commitment, Coordinator, NodeId, PrincipalId, ResourceScope,
    Right, RowCommandApplication, RowSelection, Scope, SqlValue, StrongHead, StrongPackage,
    StrongReceipt, StrongRequest,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
pub use ticket::{Custody, Delivery, MAX_OPEN_TICKETS, Ticket, TicketResult, TicketState};

/// The contiguous prefix of one scope's confirmed history that was verified
/// against its projection since open, and the rows it leaves.
pub(super) struct Audited {
    authority: u64,
    manifest: Commitment,
    sequence: u64,
    head: Option<Commitment>,
    rows: BTreeMap<Commitment, rocky_core::RowFields>,
}

pub fn secure_node_id(key: PublicKey) -> NodeId {
    let hash = rocky_core::commitment("rocky.secure-node.v1", key.as_bytes());
    let mut id = [0; 16];
    id.copy_from_slice(&hash[..16]);
    NodeId(id)
}
pub fn secure_principal_id(key: PublicKey) -> PrincipalId {
    let hash = rocky_core::commitment("rocky.secure-principal.v1", key.as_bytes());
    let mut id = [0; 16];
    id.copy_from_slice(&hash[..16]);
    PrincipalId(id)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EncryptedCommand {
    pub(super) request: Vec<u8>,
    node: PublicKey,
    credentials: CredentialChain,
    signature: Signature,
}
impl EncryptedCommand {
    pub fn request(&self) -> Result<StrongRequest, Error> {
        Ok(StrongRequest::decode(&self.request)?)
    }
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        bounded(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode(bytes)
    }
    pub(super) fn verify<A: RowCommandApplication>(
        &self,
        inner: &Inner<A>,
    ) -> Result<StrongRequest, Error> {
        self.verify_at(inner, &inner.config.policy.statement, now()?)
    }
    fn verify_at<A: RowCommandApplication>(
        &self,
        inner: &Inner<A>,
        policy: &PolicyStatement,
        time: u64,
    ) -> Result<StrongRequest, Error> {
        let request = self.request()?;
        if !self.node.verify(
            &signing_bytes(
                "rocky.encrypted-command.v1",
                &(&self.request, &self.credentials),
            )?,
            &self.signature,
        ) || secure_node_id(self.node) != request.id.node
            || self
                .credentials
                .0
                .first()
                .is_none_or(|cap| secure_principal_id(cap.capability.subject) != request.caller)
            || request.scope.app != inner.definition.app
            || request.digest != request.policy
            || !request.frontier.0.is_empty()
        {
            return Err(denied("invalid encrypted command identity/generation"));
        }
        // A command submitted under an earlier generation inside the
        // horizon keeps its signed identity and runs under this one.
        let live = *policy == inner.config.policy.statement;
        let submitted = inner.generation_of(request.policy, live)?;
        if request.schema != submitted.schema
            || request.handler != submitted.handler
            || self
                .credentials
                .0
                .first()
                .is_some_and(|root| root.capability.manifest != request.policy)
        {
            return Err(denied("invalid encrypted command identity/generation"));
        }
        if !inner
            .definition
            .methods
            .iter()
            .any(|method| method.name == request.method)
        {
            return Err(denied("unregistered command"));
        }
        // No translation between versions: one outside the horizon is refused.
        if !inner.app.supports_version(&request.method, request.version) {
            return Err(Error::Incompatible(format!(
                "{} version {} is outside the support horizon",
                request.method, request.version
            )));
        }
        let grants = inner.verify_chain_with(&self.credentials, self.node, policy, time)?;
        let invocation: EncryptedPackage = decode(&request.arguments)?;
        let resources = resources(inner, &request)?;
        if invocation.generation == 0
            || invocation.generation > i64::MAX as u64
            || invocation.kind != PackageKind::Invocation
            || !resources.contains(&invocation.scope)
            || invocation.scope
                != inner
                    .app
                    .command_invocation_scope(request.scope, &request.method)
                    .map_err(|message| denied(&message))?
        {
            return Err(denied("invalid invocation scope"));
        }
        let method = inner.definition.item_id("method", &request.method);
        if !grants.iter().any(|grant| {
            grant.scope == invocation.scope
                && grant.rows == RowSelection::All
                && grant.rights.contains(&Right::Invoke)
                && grant.fields.contains(&method)
        }) {
            return Err(denied("command invocation denied"));
        }
        Ok(request)
    }
}

/// The receipt binds every ciphertext commitment; parts may contain an authorized subset.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ConfirmedCommand {
    pub(super) command: EncryptedCommand,
    package: Vec<u8>,
    pub(super) receipt: Vec<u8>,
    executor: PublicKey,
    credentials: CredentialChain,
    signature: Signature,
    pub parts: Vec<EncryptedPackage>,
}
impl ConfirmedCommand {
    pub fn to_bytes(&self) -> Result<Vec<u8>, Error> {
        bounded(self)
    }
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
        decode(bytes)
    }
    pub fn strong_package(&self) -> Result<StrongPackage, Error> {
        Ok(StrongPackage::decode(&self.package)?)
    }
    pub fn receipt(&self) -> Result<StrongReceipt, Error> {
        Ok(StrongReceipt::decode(&self.receipt)?)
    }
    fn signature_bytes(&self) -> Result<Vec<u8>, Error> {
        signing_bytes(
            "rocky.encrypted-result.v1",
            &(&self.command, &self.package, &self.credentials),
        )
    }
    pub(super) fn verify<A: RowCommandApplication>(
        &self,
        inner: &Inner<A>,
    ) -> Result<StrongPackage, Error> {
        self.verify_at(inner, &inner.config.policy.statement, now()?)
    }
    pub(super) fn verify_preparation<A: RowCommandApplication>(
        &self,
        inner: &Inner<A>,
    ) -> Result<(), Error> {
        let package = self.strong_package()?;
        let mut candidate = self.clone();
        candidate.receipt = StrongReceipt {
            confirmation: package.confirmation()?,
            sequence: package.sequence,
        }
        .encode();
        candidate.verify(inner)?;
        let scopes = resources(inner, &package.request)?;
        if scopes.iter().any(|scope| {
            !candidate
                .parts
                .iter()
                .any(|part| part.scope == *scope && part.kind == PackageKind::Rows)
        }) {
            return Err(denied("preparation missing invariant projection"));
        }
        Ok(())
    }
    pub(super) fn verify_history<A: RowCommandApplication>(
        &self,
        inner: &Inner<A>,
    ) -> Result<StrongPackage, Error> {
        let (policy, time) = self.historical(inner)?;
        self.verify_at(inner, &policy, time)
    }
    /// The caller's and executor's grants under the policy epoch and time
    /// `verify_history` checks a result at.
    pub(super) fn historical_grants<A: RowCommandApplication>(
        &self,
        inner: &Inner<A>,
    ) -> Result<
        (
            Vec<rocky_core::ResourceGrant>,
            Vec<rocky_core::ResourceGrant>,
        ),
        Error,
    > {
        let (policy, time) = self.historical(inner)?;
        Ok((
            inner.verify_chain_with(&self.command.credentials, self.command.node, &policy, time)?,
            inner.verify_chain_with(&self.credentials, self.executor, &policy, time)?,
        ))
    }
    fn historical<A: RowCommandApplication>(
        &self,
        inner: &Inner<A>,
    ) -> Result<(PolicyStatement, u64), Error> {
        let leaf = &self
            .command
            .credentials
            .0
            .last()
            .ok_or_else(|| denied("missing historical credential"))?
            .capability;
        let policy = PolicyStatement {
            manifest: inner.manifest,
            epoch: leaf.epoch,
            revoked: BTreeSet::new(),
        };
        let executor_time = self
            .credentials
            .0
            .last()
            .ok_or_else(|| denied("missing historical executor credential"))?
            .capability
            .not_before;
        Ok((policy, leaf.not_before.max(executor_time)))
    }
    fn verify_at<A: RowCommandApplication>(
        &self,
        inner: &Inner<A>,
        policy: &PolicyStatement,
        time: u64,
    ) -> Result<StrongPackage, Error> {
        let request = self.command.verify_at(inner, policy, time)?;
        let package = self.strong_package()?;
        let receipt = self.receipt()?;
        let grants = inner.verify_chain_with(&self.credentials, self.executor, policy, time)?;
        if request != package.request
            || package.base.executor != secure_node_id(self.executor)
            || !self
                .executor
                .verify(&self.signature_bytes()?, &self.signature)
            || package.confirmation()? != receipt.confirmation
            || package.sequence != receipt.sequence
            || !package.required.is_empty()
        {
            return Err(denied("invalid encrypted command result"));
        }
        let resources = resources(inner, &request)?;
        for scope in &resources {
            if !inner.can_read_key(*scope, &grants)
                || !grants.iter().any(|grant| {
                    grant.scope == *scope
                        && grant.rows == RowSelection::All
                        && grant.rights.contains(&Right::Execute)
                        && grant
                            .fields
                            .contains(&inner.definition.item_id("method", &request.method))
                })
            {
                return Err(denied("executor lacks complete invariant state/authority"));
            }
        }
        let binding = PackageSet::from_bytes(&package.rows)?;
        binding.verify_subset(&self.parts)?;
        let mut seen = BTreeSet::new();
        let mut row_scopes = BTreeSet::new();
        for part in &self.parts {
            if part.generation == 0
                || part.generation > i64::MAX as u64
                || !resources.contains(&part.scope)
                || !seen.insert(part.commitment()?)
                || part.kind == PackageKind::Invocation
                || (part.kind == PackageKind::Rows && !row_scopes.insert(part.scope))
            {
                return Err(denied("invalid result scope/kind"));
            }
        }
        Ok(package)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct RowProjection {
    pub(crate) before: BTreeMap<Commitment, rocky_core::RowFields>,
    pub(crate) after: BTreeMap<Commitment, rocky_core::RowFields>,
}
fn bounded<T: Serialize>(value: &T) -> Result<Vec<u8>, Error> {
    let bytes = encode(value)?;
    if bytes.len() > MAX_SECURE_FRAME / 2 {
        return Err(denied("command envelope quota"));
    }
    Ok(bytes)
}
fn validate_effects<A: RowCommandApplication>(
    inner: &Inner<A>,
    changes: &[rocky_core::RowChange],
) -> Result<(), Error> {
    for change in changes {
        for fields in change.before.iter().chain(change.after.iter()) {
            if fields.len() > 256 || fields.keys().any(|field| !inner.fields.contains(field)) {
                return Err(denied("command effect field outside signed schema"));
            }
        }
    }
    Ok(())
}
pub(super) fn resources<A: RowCommandApplication>(
    inner: &Inner<A>,
    request: &StrongRequest,
) -> Result<Vec<ResourceScope>, Error> {
    if !inner
        .definition
        .methods
        .iter()
        .any(|method| method.name == request.method)
    {
        return Err(denied("unregistered command"));
    }
    let scopes = inner
        .app
        .command_resources(request.scope, &request.method)
        .map_err(|message| denied(&message))?;
    if scopes.is_empty()
        || scopes.len() > 32
        || scopes
            .iter()
            .any(|scope| scope.domain != request.scope || !inner.tables.contains(&scope.table))
        || scopes.iter().copied().collect::<BTreeSet<_>>().len() != scopes.len()
    {
        return Err(denied("invalid command invariant scopes"));
    }
    Ok(scopes)
}

/// Serializes command execution on one replica. Executions beyond the
/// waiting bound are refused instead of queued.
#[derive(Default)]
pub(crate) struct Gate {
    lock: tokio::sync::Mutex<()>,
    waiting: std::sync::atomic::AtomicUsize,
}
/// Executions that may wait for the one that runs.
const MAX_WAITING: usize = 32;
impl Gate {
    pub(super) async fn enter(&self) -> Result<tokio::sync::MutexGuard<'_, ()>, Error> {
        use std::sync::atomic::Ordering;
        struct Waiting<'a>(&'a std::sync::atomic::AtomicUsize);
        impl Drop for Waiting<'_> {
            fn drop(&mut self) {
                self.0.fetch_sub(1, Ordering::SeqCst);
            }
        }
        if self.waiting.fetch_add(1, Ordering::SeqCst) >= MAX_WAITING {
            self.waiting.fetch_sub(1, Ordering::SeqCst);
            return Err(Error::Backpressure);
        }
        let _waiting = Waiting(&self.waiting);
        Ok(self.lock.lock().await)
    }
}

impl<A: RowCommandApplication> EncryptedReplica<A> {
    /// Commits an immutable signed invocation before returning it to a transport.
    pub async fn invoke_encrypted(
        &self,
        handle: Commitment,
        scope: ResourceScope,
        method: &str,
        arguments: &[u8],
    ) -> Result<EncryptedCommand, Error> {
        self.invoke_version(handle, scope, method, 1, arguments, None)
            .await
    }
    /// `invoke_encrypted` for a version of the method. `open` bounds the
    /// unresolved tickets a new one may join.
    pub(super) async fn invoke_version(
        &self,
        handle: Commitment,
        scope: ResourceScope,
        method: &str,
        version: u32,
        arguments: &[u8],
        open: Option<usize>,
    ) -> Result<EncryptedCommand, Error> {
        let mut inner = self.inner.lock().await;
        // Version 1 keeps the input identity of handles committed before
        // versions existed.
        let input = if version == 1 {
            rocky_core::commitment(
                "rocky.command-input.v1",
                &encode(&(scope, method, arguments))?,
            )
        } else {
            rocky_core::commitment(
                "rocky.command-input.v2",
                &encode(&(scope, method, version, arguments))?,
            )
        };
        let retained = collect_rows(
            inner
                .conn
                .query(
                    "SELECT input,command FROM secure_commands WHERE handle=? UNION ALL SELECT input,command FROM secure_recovered_commands WHERE handle=?",
                    [handle.to_vec(), handle.to_vec()],
                )
                .await?,
        )
        .await?;
        if let Some([SqlValue::Blob(old), SqlValue::Blob(bytes)]) =
            retained.first().map(Vec::as_slice)
        {
            if old.as_slice() != input {
                return Err(Error::Conflict);
            }
            let command: EncryptedCommand = decode(bytes)?;
            command.verify(&inner)?;
            inner.flush()?;
            return Ok(command);
        }
        inner.check_pruned_handle(handle, Some(input)).await?;
        if !inner.app.supports_version(method, version) {
            return Err(Error::Incompatible(format!(
                "{method} version {version} is outside the support horizon"
            )));
        }
        inner
            .app
            .validate_arguments(method, version, arguments)
            .map_err(|message| denied(&message))?;
        let command = transaction!(inner, |inner| {
            if let Some(limit) = open
                && inner.open_tickets().await? >= limit
            {
                return Err(Error::Backpressure);
            }
            let mut count = inner
                .conn
                .query("SELECT MAX(sequence) FROM secure_command_counter", ())
                .await?;
            let previous = count
                .next()
                .await?
                .ok_or_else(|| denied("missing command counter"))?
                .get::<i64>(0)?;
            // The sequence carries its issue time for bounded coordinators.
            let sequence = rocky_core::next_request_sequence(previous as u64, now()?)?.get() as i64;
            let command =
                inner.sign_command(handle, scope, method, version, arguments, sequence)?;
            command.verify(inner)?;
            inner.command_budget(command.to_bytes()?.len()).await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_commands VALUES(?,?,?,?)",
                    bindings(vec![
                        SqlValue::Blob(handle.to_vec()),
                        SqlValue::Blob(input.to_vec()),
                        SqlValue::Integer(sequence),
                        SqlValue::Blob(command.to_bytes()?),
                    ]),
                )
                .await?;
            inner
                .conn
                .execute("UPDATE secure_command_counter SET sequence=?", [sequence])
                .await?;
            inner
                .conn
                .execute(
                    "INSERT INTO secure_tickets VALUES(?,?)",
                    [handle.to_vec(), command.request()?.hash()?.to_vec()],
                )
                .await?;
            Ok(command)
        })?;
        // The invocation is sent to an executor or coordinator next.
        inner.flush()?;
        Ok(command)
    }
    /// A signed invocation that skips local validation and is not stored:
    /// what a peer with valid keys and broken or hostile code could send.
    #[cfg(test)]
    pub(crate) async fn forge_command(
        &self,
        handle: Commitment,
        scope: ResourceScope,
        method: &str,
        version: u32,
        arguments: &[u8],
    ) -> Result<EncryptedCommand, Error> {
        let inner = self.inner.lock().await;
        let sequence = rocky_core::next_request_sequence(1 << 40, now()?)?.get() as i64;
        inner.sign_command(handle, scope, method, version, arguments, sequence)
    }
}
impl<A: RowCommandApplication> Inner<A> {
    /// Seals `arguments` for `scope` and signs the request with this node.
    fn sign_command(
        &self,
        handle: Commitment,
        scope: ResourceScope,
        method: &str,
        version: u32,
        arguments: &[u8],
        sequence: i64,
    ) -> Result<EncryptedCommand, Error> {
        let key = self
            .config
            .keys
            .iter()
            .find(|key| key.scope == scope)
            .ok_or_else(|| denied("invocation key unavailable"))?;
        let args = EncryptedPackage::seal(
            scope,
            key.generation,
            PackageKind::Invocation,
            &key.key,
            arguments,
        )?;
        let request = StrongRequest {
            scope: scope.domain,
            caller: secure_principal_id(
                self.config
                    .credentials
                    .0
                    .first()
                    .ok_or_else(|| denied("missing user credential"))?
                    .capability
                    .subject,
            ),
            id: rocky_core::RequestId {
                node: secure_node_id(self.config.node.public_key()),
                sequence: AuthorSequence::new(sequence as u64)?,
            },
            method: method.into(),
            version,
            schema: self.definition.schema,
            handler: self.definition.handler,
            client_request: handle,
            arguments: encode(&args)?,
            policy: self.manifest,
            frontier: Default::default(),
            digest: self.manifest,
        };
        let request = request.encode()?;
        let credentials = self.config.credentials.clone();
        let signature = self.config.node.sign(&signing_bytes(
            "rocky.encrypted-command.v1",
            &(&request, &credentials),
        )?);
        Ok(EncryptedCommand {
            request,
            node: self.config.node.public_key(),
            credentials,
            signature,
        })
    }
}
impl<A: RowCommandApplication> Inner<A> {
    pub(super) async fn command_budget(&self, incoming: usize) -> Result<(), Error> {
        let mut result = self.conn.query("SELECT (SELECT COUNT(*) FROM secure_commands)+(SELECT COUNT(*) FROM secure_recovered_commands)+(SELECT COUNT(*) FROM secure_preparations)+(SELECT COUNT(*) FROM secure_results), (SELECT COALESCE(SUM(LENGTH(command)),0) FROM secure_commands)+(SELECT COALESCE(SUM(LENGTH(command)),0) FROM secure_recovered_commands)+(SELECT COALESCE(SUM(LENGTH(result)),0) FROM secure_preparations)+(SELECT COALESCE(SUM(LENGTH(result)),0) FROM secure_results)", ()).await?;
        let row = result
            .next()
            .await?
            .ok_or_else(|| denied("missing command budget"))?;
        if row.get::<i64>(0)? >= MAX_SECURE_RECORDS as i64
            || row.get::<i64>(1)? as usize + incoming > MAX_SECURE_BYTES
        {
            return Err(denied("retained command quota"));
        }
        Ok(())
    }
    async fn command_rows(
        &self,
        scope: ResourceScope,
    ) -> Result<BTreeMap<Commitment, rocky_core::RowFields>, Error> {
        let mut result = self
            .conn
            .query(
                "SELECT rows FROM secure_command_rows WHERE scope=?",
                [encode(&scope)?],
            )
            .await?;
        match result.next().await? {
            Some(row) => decode(&row.get::<Vec<u8>>(0)?),
            None => Ok(BTreeMap::new()),
        }
    }
}

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_strong_base(scope BLOB PRIMARY KEY,state BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_strong_prune_cursor(domain BLOB PRIMARY KEY,sequence INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_strong_horizon(domain BLOB PRIMARY KEY,sequence INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_pruned_commands(handle BLOB PRIMARY KEY,input BLOB NOT NULL,node BLOB NOT NULL,sequence INTEGER NOT NULL,domain BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_command_counter(sequence INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_durability(domain BLOB PRIMARY KEY,policy BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_recovery_custody(id BLOB PRIMARY KEY,package BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_effect_observations(id BLOB PRIMARY KEY,job BLOB NOT NULL,observation BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_recovered_commands(handle BLOB PRIMARY KEY,input BLOB NOT NULL,command BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_result_ack(peer BLOB NOT NULL,domain BLOB NOT NULL,sequence INTEGER NOT NULL,scope BLOB NOT NULL,applied INTEGER NOT NULL,PRIMARY KEY(peer,domain,sequence,scope))",
    "CREATE TABLE IF NOT EXISTS secure_commands(handle BLOB PRIMARY KEY,input BLOB NOT NULL,sequence INTEGER NOT NULL UNIQUE,command BLOB NOT NULL)",
    "INSERT INTO secure_command_counter SELECT COALESCE(MAX(sequence),0) FROM secure_commands WHERE NOT EXISTS(SELECT 1 FROM secure_command_counter)",
    "CREATE TABLE IF NOT EXISTS secure_preparation_requests(preparation BLOB PRIMARY KEY,request BLOB NOT NULL,domain BLOB NOT NULL,sequence INTEGER NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_preparations(request BLOB PRIMARY KEY,result BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_results(domain BLOB NOT NULL,sequence INTEGER NOT NULL,result BLOB NOT NULL,PRIMARY KEY(domain,sequence))",
    "CREATE TABLE IF NOT EXISTS secure_command_rows(scope BLOB PRIMARY KEY,sequence INTEGER NOT NULL,head BLOB NOT NULL,rows BLOB NOT NULL)",
    // Tickets index the requests this node submitted; progress records what
    // this node knows of any request it submitted, prepared or applied.
    "CREATE TABLE IF NOT EXISTS secure_tickets(handle BLOB PRIMARY KEY,request BLOB NOT NULL UNIQUE)",
    "CREATE TABLE IF NOT EXISTS secure_command_progress(request BLOB PRIMARY KEY,state INTEGER NOT NULL,sequence INTEGER)",
];
