use crate::{
    certified::*,
    machine::{MAX_PUBLIC_DOMAINS, Request},
    raft_store::atomic_write,
    service::{Operation, Outcome, ReceiptEvidence, Registration, Reply, Status},
    transport::*,
};
use fs2::FileExt;
use p2panda_core::{PrivateKey, PublicKey};
use rocky_core::{
    net::{Protocol, Transport},
    *,
};
use rocky_credentials::{SignedCoordinationGrant, SignedTenantAdmission};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;
pub struct NetworkCoordinator {
    genesis: CertifiedGenesis,
    key: PrivateKey,
    evidence_path: PathBuf,
    cache: Mutex<ClientCache>,
    /// The client lane of a public service; voter TCP when absent.
    route: Option<Arc<dyn Transport>>,
    _lock: File,
}
#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;
/// Receipt evidence a client keeps. Beyond it the oldest evidence is
/// evicted, except for this client's own requests it has not acknowledged:
/// then new evidence waits with `Backpressure`. Applications export what
/// they retain with their results.
pub const MAX_EVIDENCE: usize = 4096;
/// One encoded `ReceiptEvidence` a peer may hand over.
const MAX_EVIDENCE_BYTES: usize = 16 * 1024;
/// Current evidence cache format, including acknowledged floors.
const CACHE_FORMAT: u32 = 3;
#[derive(Clone, Serialize, Deserialize)]
struct ClientCache {
    format: u32,
    genesis: Commitment,
    receipts: Vec<ReceiptEvidence>,
    heads: Vec<StrongHead>,
    /// Per scope, the request sequence through which this client retired
    /// its own identities. Its evidence above it is pinned.
    settled: Vec<(Scope, u64)>,
}
impl ClientCache {
    fn read(data: &[u8]) -> Result<Self, CoordinationError> {
        #[derive(Deserialize)]
        struct Format {
            format: u32,
        }
        if data.len() > MAX_STORE {
            return Err(CoordinationError::Denied);
        }
        let format: Format = p2panda_core::cbor::decode_cbor(data)
            .map_err(|_| CoordinationError::UnsupportedFormat)?;
        if format.format != CACHE_FORMAT {
            return Err(CoordinationError::UnsupportedFormat);
        }
        decode(data)
    }
    /// The floor through which `own` acknowledged its identities in `scope`.
    fn floor(&self, scope: Scope) -> u64 {
        self.settled
            .iter()
            .find(|(settled, _)| *settled == scope)
            .map_or(0, |(_, through)| *through)
    }
    fn settle(&mut self, scope: Scope, through: u64) {
        match self
            .settled
            .iter_mut()
            .find(|(settled, _)| *settled == scope)
        {
            Some((_, floor)) => *floor = (*floor).max(through),
            None => self.settled.push((scope, through)),
        }
    }
    /// Keeps the newest evidence of every scope, the regression watermark,
    /// and the evidence of `own` requests above their acknowledged floor,
    /// the only proof of an outcome this client may still have to settle.
    fn insert(&mut self, evidence: ReceiptEvidence, own: NodeId) -> Result<(), CoordinationError> {
        if self
            .receipts
            .iter()
            .any(|old| old.receipt == evidence.receipt)
        {
            return Ok(());
        }
        if self.receipts.len() >= MAX_EVIDENCE {
            let mut newest = BTreeMap::<Scope, u64>::new();
            for receipt in &self.receipts {
                let sequence = newest
                    .entry(receipt.receipt.confirmation.scope)
                    .or_default();
                *sequence = (*sequence).max(receipt.receipt.sequence);
            }
            let kept = |e: &ReceiptEvidence| {
                let confirmation = &e.receipt.confirmation;
                let newest = newest.get(&confirmation.scope) == Some(&e.receipt.sequence);
                let pinned = confirmation.request.node == own
                    && confirmation.request.sequence.get() > self.floor(confirmation.scope);
                newest || pinned
            };
            let index = self
                .receipts
                .iter()
                .position(|e| !kept(e))
                .ok_or(CoordinationError::Backpressure)?;
            self.receipts.remove(index);
        }
        self.receipts.push(evidence);
        Ok(())
    }
}
impl NetworkCoordinator {
    pub fn open(
        genesis: CertifiedGenesis,
        root: PublicKey,
        mesh: MeshId,
        key: PrivateKey,
        evidence_path: PathBuf,
    ) -> Result<Self, CoordinationError> {
        genesis.verify(root, mesh)?;
        let evidence_path = if evidence_path.is_absolute() {
            evidence_path
        } else {
            std::env::current_dir()
                .map_err(|_| CoordinationError::Corrupt)?
                .join(evidence_path)
        };
        if evidence_path.exists() {
            ClientCache::read(
                &std::fs::read(&evidence_path).map_err(|_| CoordinationError::Corrupt)?,
            )?;
        }
        if let Some(parent) = evidence_path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| CoordinationError::Corrupt)?;
        }
        let mut lock_name = evidence_path
            .file_name()
            .ok_or(CoordinationError::Corrupt)?
            .to_os_string();
        lock_name.push(".lock");
        let lock = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(evidence_path.with_file_name(lock_name))
            .map_err(|_| CoordinationError::Corrupt)?;
        lock.try_lock_exclusive()
            .map_err(|_| CoordinationError::Denied)?;
        let cache = if evidence_path.exists() {
            ClientCache::read(
                &std::fs::read(&evidence_path).map_err(|_| CoordinationError::Corrupt)?,
            )?
        } else {
            ClientCache {
                format: CACHE_FORMAT,
                genesis: genesis.id()?,
                receipts: Vec::new(),
                heads: Vec::new(),
                settled: Vec::new(),
            }
        };
        let public = genesis.genesis.public.is_some();
        // A cache bound to another genesis is another service's: changing
        // service is a migration, never an address change.
        if cache.genesis != genesis.id()?
            || cache.receipts.len() > MAX_EVIDENCE
            || cache.heads.len() > MAX_PUBLIC_DOMAINS
            || cache.heads.iter().any(|head| {
                (!public && head.scope.mesh != mesh)
                    || head.configuration != genesis.genesis.configuration
            })
        {
            return Err(CoordinationError::Configuration);
        }
        for evidence in &cache.receipts {
            evidence.verify(&genesis)?;
        }
        Ok(Self {
            genesis,
            key,
            evidence_path,
            cache: Mutex::new(cache),
            route: None,
            _lock: lock,
        })
    }
    /// A client of the public service `bundle` pins, reached through
    /// `transport` at the certified client endpoints of its kind. Requests
    /// are signed with `key`; the transport stays its owner's.
    pub fn connect(
        bundle: &TrustBundle,
        key: PrivateKey,
        evidence_path: PathBuf,
        transport: Arc<dyn Transport>,
    ) -> Result<Self, CoordinationError> {
        let genesis = bundle.genesis()?;
        let public = genesis
            .genesis
            .public
            .as_ref()
            .ok_or(CoordinationError::Configuration)?;
        if !public.endpoints.iter().any(|e| e.kind == transport.kind()) {
            return Err(CoordinationError::Configuration);
        }
        let mut client = Self::open(genesis, bundle.root, bundle.mesh, key, evidence_path)?;
        client.route = Some(transport);
        Ok(client)
    }
    /// One attempt at each reachable voter, in certified order.
    async fn attempt(&self, request: &Request) -> Vec<Result<Reply, CoordinationError>> {
        let genesis = match self.genesis.id() {
            Ok(id) => id,
            Err(error) => return vec![Err(error)],
        };
        let message = || Message::Client(Box::new(request.clone()));
        let mut replies = Vec::new();
        match (&self.route, &self.genesis.genesis.public) {
            (Some(transport), Some(public)) => {
                for endpoint in public
                    .endpoints
                    .iter()
                    .filter(|e| e.kind == transport.kind())
                {
                    let Ok(voter) = self.voter(endpoint.voter) else {
                        continue;
                    };
                    let reply = tokio::time::timeout(std::time::Duration::from_secs(3), async {
                        let mut stream = transport
                            .connect(&endpoint.address, Protocol::Coordinate)
                            .await
                            .map_err(|_| CoordinationError::Unavailable)?;
                        converse(&mut stream, voter.key, genesis, &self.key, message()).await
                    })
                    .await
                    .unwrap_or(Err(CoordinationError::Unavailable));
                    if reply.is_err() {
                        transport.forget(&endpoint.address);
                    }
                    let done = reply
                        .as_ref()
                        .is_ok_and(|r: &Reply| r.result != Err(CoordinationError::Unavailable));
                    replies.push(reply);
                    if done {
                        break;
                    }
                }
            }
            _ => {
                for voter in &self.genesis.genesis.voters {
                    let reply = exchange(voter, genesis, &self.key, message()).await;
                    let done = reply
                        .as_ref()
                        .is_ok_and(|r: &Reply| r.result != Err(CoordinationError::Unavailable));
                    replies.push(reply);
                    if done {
                        break;
                    }
                }
            }
        }
        replies
    }
    fn persist(&self, cache: &ClientCache) -> Result<(), CoordinationError> {
        persist_cache(&self.evidence_path, cache)
    }
    pub async fn retain_evidence(
        &self,
        evidence: ReceiptEvidence,
    ) -> Result<(), CoordinationError> {
        evidence.verify(&self.genesis)?;
        let mut cache = self.cache.lock().await;
        if !cache
            .receipts
            .iter()
            .any(|old| old.receipt == evidence.receipt)
        {
            let mut updated = cache.clone();
            updated.insert(evidence, node_id(self.key.public_key()))?;
            self.persist(&updated)?;
            *cache = updated;
        }
        Ok(())
    }
    pub async fn evidence(&self, receipt: &StrongReceipt) -> Option<ReceiptEvidence> {
        self.cache
            .lock()
            .await
            .receipts
            .iter()
            .find(|e| e.receipt == *receipt)
            .cloned()
    }
    pub async fn call(
        &self,
        id: Commitment,
        operation: Operation,
    ) -> Result<Outcome, CoordinationError> {
        let live_head = matches!(operation, Operation::Head { .. });
        let request = Request::sign(self.genesis.id()?, id, operation, &self.key)?;
        tokio::time::timeout(std::time::Duration::from_secs(12), async {
            for _ in 0..3 {
                for response in self.attempt(&request).await {
                    let reply = match response {
                        Ok(reply) if reply.result != Err(CoordinationError::Unavailable) => reply,
                        _ => continue,
                    };
                    if let Ok(Outcome::Receipt(Some(receipt))) = &reply.result
                        && reply
                            .evidence
                            .as_ref()
                            .is_none_or(|e| e.receipt != *receipt)
                    {
                        return Err(CoordinationError::InvalidCommitment);
                    }
                    if let Some(evidence) = reply.evidence {
                        self.retain_evidence(evidence).await?;
                    }
                    if let Ok(Outcome::Head(head)) = &reply.result
                        && live_head
                    {
                        self.observe_head(head).await?;
                    }
                    return reply.result;
                }
                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            }
            Err(CoordinationError::Unavailable)
        })
        .await
        .map_err(|_| CoordinationError::Unavailable)?
    }
    pub async fn acquire(
        &self,
        scope: Scope,
        id: Commitment,
    ) -> Result<StrongHead, CoordinationError> {
        match self.call(id, Operation::Acquire { scope }).await? {
            Outcome::Head(head) => Ok(head),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    pub async fn takeover(
        &self,
        scope: Scope,
        expected_epoch: u64,
        id: Commitment,
    ) -> Result<StrongHead, CoordinationError> {
        match self
            .call(
                id,
                Operation::Takeover {
                    scope,
                    expected_epoch,
                },
            )
            .await?
        {
            Outcome::Head(head) => Ok(head),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    pub async fn replace_domain(
        &self,
        record: crate::ReplacementRecord,
        certificate: ControlCertificate,
        id: Commitment,
    ) -> Result<crate::ReplacementRecord, CoordinationError> {
        match self
            .call(
                id,
                Operation::Replace {
                    record,
                    certificate,
                },
            )
            .await?
        {
            Outcome::Replacement(record) => Ok(record),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// Registers the domain of `certificate` on a public service under this
    /// client's key, which `grant` names as the application's tenant.
    pub async fn register(
        &self,
        grant: SignedCoordinationGrant,
        admission: Option<SignedTenantAdmission>,
        certificate: ControlCertificate,
        id: Commitment,
    ) -> Result<Commitment, CoordinationError> {
        match self
            .call(
                id,
                Operation::Register(Box::new(Registration {
                    grant,
                    admission,
                    certificate,
                })),
            )
            .await?
        {
            Outcome::Controlled(reference) => Ok(reference),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// A new control revision: executors, invokers or validity. It fences
    /// the current executor and preserves the head.
    pub async fn control(
        &self,
        certificate: ControlCertificate,
        id: Commitment,
    ) -> Result<Commitment, CoordinationError> {
        match self.call(id, Operation::Control(certificate)).await? {
            Outcome::Controlled(reference) => Ok(reference),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// This tenant's use of its budgets.
    pub async fn usage(&self) -> Result<crate::service::Usage, CoordinationError> {
        match self.call(nonce(), Operation::Usage).await? {
            Outcome::Usage(usage) => Ok(usage),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    async fn observe_head(&self, head: &StrongHead) -> Result<(), CoordinationError> {
        let mut cache = self.cache.lock().await;
        // Retained evidence at this head is the same durable watermark.
        let mut covered = false;
        if let Some(receipt) = cache
            .receipts
            .iter()
            .filter(|e| e.receipt.confirmation.scope == head.scope)
            .max_by_key(|e| e.receipt.sequence)
        {
            let p = &receipt.receipt.confirmation;
            if head.sequence < receipt.receipt.sequence
                || head.epoch < p.epoch
                || (head.sequence == receipt.receipt.sequence && head.head != p.new_head)
            {
                return Err(CoordinationError::CoordinationStateLost);
            }
            covered = head.sequence == receipt.receipt.sequence && head.epoch == p.epoch;
        }
        let known = cache.heads.iter().position(|h| h.scope == head.scope);
        if let Some(old) = known.map(|index| &cache.heads[index])
            && (head.sequence < old.sequence
                || head.epoch < old.epoch
                || (head.sequence == old.sequence && head.head != old.head))
        {
            return Err(CoordinationError::CoordinationStateLost);
        }
        if known.is_none() && cache.heads.len() >= MAX_PUBLIC_DOMAINS {
            return Err(CoordinationError::Configuration);
        }
        if known.is_none_or(|index| cache.heads[index] != *head) {
            let set = |cache: &mut ClientCache| match known {
                Some(index) => cache.heads[index] = head.clone(),
                None => cache.heads.push(head.clone()),
            };
            // The whole cache is one file: it is written only
            // for a watermark no retained evidence records.
            if covered {
                set(&mut cache);
            } else {
                let mut updated = cache.clone();
                set(&mut updated);
                self.persist(&updated)?;
                *cache = updated;
            }
        }
        Ok(())
    }
    /// Owner-initiated closure of `head.scope` at exactly `head`.
    pub async fn close_domain(
        &self,
        head: StrongHead,
        id: Commitment,
    ) -> Result<StrongHead, CoordinationError> {
        match self.call(id, Operation::Close { head }).await? {
            Outcome::Head(head) => {
                let mut cache = self.cache.lock().await;
                let mut updated = cache.clone();
                updated.heads.retain(|old| old.scope != head.scope);
                self.persist(&updated)?;
                *cache = updated;
                Ok(head)
            }
            _ => Err(CoordinationError::Corrupt),
        }
    }
    /// The scope authority's activation of the next definition generation
    /// at exactly `activation.head`. Retrying the same activation, with the
    /// same `id`, returns the activated head.
    pub async fn activate(
        &self,
        activation: Activation,
        id: Commitment,
    ) -> Result<StrongHead, CoordinationError> {
        match self.call(id, Operation::Activate(activation)).await? {
            Outcome::Head(head) => Ok(head),
            _ => Err(CoordinationError::Corrupt),
        }
    }
    pub async fn status(&self, voter: u64) -> Result<Status, CoordinationError> {
        exchange(
            self.voter(voter)?,
            self.genesis.id()?,
            &self.key,
            Message::Status,
        )
        .await
    }
    fn voter(&self, id: u64) -> Result<&Voter, CoordinationError> {
        self.genesis
            .genesis
            .voters
            .iter()
            .find(|v| v.id == id)
            .ok_or(CoordinationError::Denied)
    }
    pub async fn initialize(&self, voter: u64) -> Result<(), CoordinationError> {
        exchange(
            self.voter(voter)?,
            self.genesis.id()?,
            &self.key,
            Message::Initialize,
        )
        .await?
    }
    pub async fn snapshot(&self, voter: u64) -> Result<(), CoordinationError> {
        exchange(
            self.voter(voter)?,
            self.genesis.id()?,
            &self.key,
            Message::SnapshotNow,
        )
        .await?
    }
}
impl Coordinator for NetworkCoordinator {
    fn head(&self, scope: Scope) -> CoordinationFuture<'_, StrongHead> {
        Box::pin(async move {
            match self.call(nonce(), Operation::Head { scope }).await? {
                Outcome::Head(head) => Ok(head),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn lookup(&self, scope: Scope, id: RequestId) -> CoordinationFuture<'_, Option<StrongReceipt>> {
        Box::pin(async move {
            match self
                .call(nonce(), Operation::Lookup { scope, request: id })
                .await?
            {
                Outcome::Receipt(receipt) => Ok(receipt),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn confirm(&self, proposal: Confirmation) -> CoordinationFuture<'_, StrongReceipt> {
        Box::pin(async move {
            let id = commitment("rocky.confirm-attempt.v1", &encode(&proposal)?);
            match self.call(id, Operation::Confirm(proposal)).await? {
                Outcome::Receipt(Some(receipt)) => Ok(receipt),
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
    fn verify<'a>(&'a self, receipt: &'a StrongReceipt) -> CoordinationFuture<'a, ()> {
        Box::pin(async move {
            if let Some(evidence) = self.evidence(receipt).await {
                return evidence.verify(&self.genesis);
            }
            // Without evidence, a retired identity's result cannot be verified.
            match self
                .lookup(receipt.confirmation.scope, receipt.confirmation.request)
                .await
            {
                Ok(Some(retained)) if retained == *receipt => Ok(()),
                Err(CoordinationError::Retired | CoordinationError::Expired) => {
                    Err(CoordinationError::Retired)
                }
                Ok(_) => Err(CoordinationError::InvalidCommitment),
                Err(error) => Err(error),
            }
        })
    }
    fn export_evidence<'a>(
        &'a self,
        receipt: &'a StrongReceipt,
    ) -> CoordinationFuture<'a, Option<Vec<u8>>> {
        Box::pin(async move {
            match self.evidence(receipt).await {
                Some(evidence) => Ok(Some(encode(&evidence)?)),
                None => Ok(None),
            }
        })
    }
    fn import_evidence<'a>(&'a self, evidence: &'a [u8]) -> CoordinationFuture<'a, ()> {
        Box::pin(async move {
            if evidence.len() > MAX_EVIDENCE_BYTES {
                return Err(CoordinationError::Denied);
            }
            self.retain_evidence(decode(evidence)?).await
        })
    }
    fn retire(&self, scope: Scope, node: NodeId, through: u64) -> CoordinationFuture<'_, ()> {
        Box::pin(async move {
            // Only the issuing node acknowledges its own identities.
            if node != node_id(self.key.public_key()) {
                return Err(CoordinationError::Denied);
            }
            match self
                .call(nonce(), Operation::Retire { scope, through })
                .await?
            {
                Outcome::Floor(_) => {
                    // Evidence through the floor is no longer pinned.
                    let mut cache = self.cache.lock().await;
                    let mut updated = cache.clone();
                    updated.settle(scope, through);
                    self.persist(&updated)?;
                    *cache = updated;
                    Ok(())
                }
                _ => Err(CoordinationError::Corrupt),
            }
        })
    }
}

fn persist_cache(path: &std::path::Path, cache: &ClientCache) -> Result<(), CoordinationError> {
    let data = encode(cache)?;
    if data.len() > MAX_STORE {
        return Err(CoordinationError::Backpressure);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| CoordinationError::Corrupt)?;
    }
    atomic_write(path, &data, "client_evidence").map_err(|_| CoordinationError::Corrupt)
}
