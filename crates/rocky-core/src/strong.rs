use crate::*;
use sha2::{Digest, Sha256};

pub const MAX_STRONG_BYTES: usize = 16 * 1024 * 1024;
/// Most operations a strong cut covers after its base.
pub const MAX_CUT_OPERATIONS: usize = 32_768;
/// The one package format; bytes without it are refused.
const PACKAGE_MAGIC: &[u8; 16] = b"rocky.package.v2";
pub type Commitment = [u8; 32];
pub type Configuration = [u8; 16];
/// Application-selected method and active generations for sealed-input commands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandMethod {
    pub name: &'static str,
    pub version: u32,
    pub schema: u32,
    pub handler: u32,
}
#[derive(
    Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, serde::Serialize, serde::Deserialize,
)]
pub struct RequestId {
    pub node: NodeId,
    pub sequence: AuthorSequence,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrongRequest {
    pub scope: Scope,
    pub caller: PrincipalId,
    pub id: RequestId,
    pub method: String,
    pub version: u32,
    pub schema: u32,
    pub handler: u32,
    pub client_request: Commitment,
    pub arguments: Vec<u8>,
    pub policy: Commitment,
    pub frontier: AppliedFrontier,
    pub digest: Commitment,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StrongHead {
    pub scope: Scope,
    pub configuration: Configuration,
    pub executor: NodeId,
    pub epoch: u64,
    pub schema: u32,
    pub handler: u32,
    pub sequence: u64,
    pub head: Commitment,
}
/// A domain's move to the next definition generation at exactly `head`.
/// The coordinator records the commitments and generations, never the
/// definitions; confirmations of other generations are refused afterwards.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Activation {
    /// The head the executor's confirmed state is complete at, with the
    /// generations it was confirmed under.
    pub head: StrongHead,
    pub schema: u32,
    pub handler: u32,
    /// The id of the publisher-signed transition.
    pub transition: Commitment,
    /// Verified invariant-relevant confirmed rows at the predecessor.
    pub state: Commitment,
}
impl Activation {
    /// A step forward: one schema generation at most, neither going back.
    pub fn valid(&self) -> bool {
        let (schema, handler) = (self.head.schema, self.head.handler);
        self.transition != [0; 32]
            && self.state != [0; 32]
            && self.schema >= schema
            && self.handler >= handler
            && (self.schema, self.handler) != (schema, handler)
            && self.schema <= schema.saturating_add(1)
    }
    /// The commitment an activation binds: the confirmed head, the new
    /// generations and the transition.
    pub fn commitment(&self) -> Commitment {
        let mut bytes = self.head.head.to_vec();
        bytes.extend(self.head.sequence.to_le_bytes());
        bytes.extend(self.schema.to_le_bytes());
        bytes.extend(self.handler.to_le_bytes());
        bytes.extend(self.transition);
        bytes.extend(self.state);
        commitment("rocky.activation.v1", &bytes)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Confirmation {
    pub caller: PrincipalId,
    pub scope: Scope,
    pub configuration: Configuration,
    pub executor: NodeId,
    pub epoch: u64,
    pub request: RequestId,
    pub request_hash: Commitment,
    pub expected_sequence: u64,
    pub expected_head: Commitment,
    pub schema: u32,
    pub handler: u32,
    pub result: Commitment,
    pub new_head: Commitment,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct StrongReceipt {
    pub confirmation: Confirmation,
    pub sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StrongPackage {
    pub request: StrongRequest,
    pub base: StrongHead,
    pub sequence: u64,
    /// Canonical immutable application-owned result bytes.
    pub rows: Vec<u8>,
    /// The checkpoint frontier the executor rebuilt the cut from. The cut
    /// covers the operations after it up to the request's frontier; see
    /// `StrongPackage::required`.
    pub cut_base: AppliedFrontier,
}
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum CoordinationError {
    Unavailable,
    CoordinationStateLost,
    UnknownDomain,
    Denied,
    Configuration,
    Generations,
    StaleEpoch,
    StaleHead,
    InvalidCommitment,
    RequestConflict,
    AlreadyConfirmed(Box<StrongReceipt>),
    Corrupt,
    /// The identity is behind a retirement floor or its response expired:
    /// it never executes again and its outcome is unavailable.
    Retired,
    /// The identity's admission window closed without a confirmation: it
    /// was not confirmed and never will be.
    Expired,
    /// A window or quota is full, or the identity is issued ahead of
    /// authority time. Retry the same identity later.
    Backpressure,
    /// A persisted or wire format this build refuses to migrate.
    UnsupportedFormat,
}
impl std::fmt::Display for CoordinationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for CoordinationError {}
/// Trusted metadata port. Calls occur outside storage transactions.
pub type CoordinationFuture<'a, T> =
    std::pin::Pin<Box<dyn std::future::Future<Output = Result<T, CoordinationError>> + Send + 'a>>;
pub trait Coordinator: Send + Sync {
    fn head(&self, scope: Scope) -> CoordinationFuture<'_, StrongHead>;
    /// Cancellation/timeout after submission leaves the outcome unknown. Lookup
    /// this request identity before attempting another confirmation.
    fn confirm(&self, proposal: Confirmation) -> CoordinationFuture<'_, StrongReceipt>;
    fn lookup(&self, scope: Scope, id: RequestId) -> CoordinationFuture<'_, Option<StrongReceipt>>;
    fn verify<'a>(&'a self, receipt: &'a StrongReceipt) -> CoordinationFuture<'a, ()>;
    /// Durable acknowledgment by the invoking `node`: every identity it
    /// issued up to `through` is settled, and may be retired.
    fn retire(&self, scope: Scope, node: NodeId, through: u64) -> CoordinationFuture<'_, ()>;
    /// The portable signed evidence of `receipt` this client retains, for a
    /// peer that receives the result after its identity may have retired.
    fn export_evidence<'a>(
        &'a self,
        _receipt: &'a StrongReceipt,
    ) -> CoordinationFuture<'a, Option<Vec<u8>>> {
        Box::pin(async { Ok(None) })
    }
    /// Verifies a peer's exported evidence offline and retains it, so
    /// `verify` accepts its receipt without a live lookup. Evidence never
    /// changes the current authoritative head.
    fn import_evidence<'a>(&'a self, _evidence: &'a [u8]) -> CoordinationFuture<'a, ()> {
        Box::pin(async { Ok(()) })
    }
}
/// Request sequences carry their issue time in whole seconds above a
/// per-second counter, so a coordinator bounds admission and retention by
/// time without remembering every identity.
pub const ISSUE_SHIFT: u32 = 24;
/// Issue time, in seconds since the Unix epoch, of a request sequence.
pub const fn issued_at(sequence: u64) -> u64 {
    sequence >> ISSUE_SHIFT
}
/// The next strictly increasing request sequence issued at `now` seconds.
pub fn next_request_sequence(previous: u64, now: u64) -> Result<AuthorSequence, ProtocolError> {
    let floor = now
        .checked_mul(1 << ISSUE_SHIFT)
        .ok_or(ProtocolError::InvalidCounter)?;
    AuthorSequence::new(
        previous
            .checked_add(1)
            .ok_or(ProtocolError::InvalidCounter)?
            .max(floor),
    )
}
pub fn commitment(prefix: &str, payload: &[u8]) -> Commitment {
    let mut hash = Sha256::new();
    hash.update((prefix.len() as u32).to_le_bytes());
    hash.update(prefix.as_bytes());
    hash.update(payload);
    hash.finalize().into()
}
pub fn state_commitment(rows: &[u8]) -> Commitment {
    commitment("rocky.mvp.state.v1", rows)
}
pub fn genesis(scope: Scope, configuration: Configuration) -> Commitment {
    let mut b = Vec::new();
    put_scope(&mut b, scope);
    b.extend(configuration);
    commitment("rocky.mvp.genesis.v1", &b)
}
pub fn successor(
    previous: Commitment,
    sequence: u64,
    request: Commitment,
    result: Commitment,
) -> Commitment {
    let mut b = previous.to_vec();
    b.extend(sequence.to_le_bytes());
    b.extend(request);
    b.extend(result);
    commitment("rocky.mvp.head.v1", &b)
}
fn put_scope(b: &mut Vec<u8>, scope: Scope) {
    b.extend(scope.mesh.0);
    b.extend(scope.app.0);
    b.extend(scope.domain.0);
}
fn put_id(b: &mut Vec<u8>, node: NodeId, sequence: AuthorSequence) {
    b.extend(node.0);
    b.extend(sequence.get().to_le_bytes());
}
fn blob(b: &mut Vec<u8>, v: &[u8]) {
    b.extend((v.len() as u32).to_le_bytes());
    b.extend(v);
}
fn frontier(b: &mut Vec<u8>, v: &AppliedFrontier) {
    b.extend((v.0.len() as u32).to_le_bytes());
    for (n, s) in &v.0 {
        put_id(b, *n, *s);
    }
}
fn valid_frontier(v: &AppliedFrontier) -> Result<(), ProtocolError> {
    if v.0.windows(2).any(|p| p[0].0 >= p[1].0) {
        return Err(ProtocolError::InvalidDependencies);
    }
    Ok(())
}
/// How many operations a cut covers after `base`: per author, those after
/// the base up to `frontier`. A base author outside the frontier, or ahead
/// of it, is refused.
pub fn cut_size(base: &AppliedFrontier, frontier: &AppliedFrontier) -> Result<u64, ProtocolError> {
    valid_frontier(base)?;
    valid_frontier(frontier)?;
    let mut size = 0u64;
    for (author, below) in &base.0 {
        match frontier.0.iter().find(|(node, _)| node == author) {
            Some((_, through)) if below <= through => {}
            _ => return Err(ProtocolError::InvalidDependencies),
        }
    }
    for (author, through) in &frontier.0 {
        let below = base
            .0
            .iter()
            .find(|(node, _)| node == author)
            .map_or(0, |(_, sequence)| sequence.get());
        size += through.get() - below;
    }
    Ok(size)
}
/// The operations a cut covers after `base`, sorted; at most
/// `MAX_CUT_OPERATIONS`.
pub fn cut_operations(
    base: &AppliedFrontier,
    frontier: &AppliedFrontier,
) -> Result<Vec<OperationId>, ProtocolError> {
    if cut_size(base, frontier)? > MAX_CUT_OPERATIONS as u64 {
        return Err(ProtocolError::InvalidLength);
    }
    let mut operations = Vec::new();
    for (node, through) in &frontier.0 {
        let below = base
            .0
            .iter()
            .find(|(author, _)| author == node)
            .map_or(0, |(_, sequence)| sequence.get());
        for sequence in below + 1..=through.get() {
            operations.push(OperationId {
                node: *node,
                sequence: AuthorSequence::new(sequence)?,
            });
        }
    }
    Ok(operations)
}
impl StrongRequest {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        valid_frontier(&self.frontier)?;
        if self.method.len() > 1024 {
            return Err(ProtocolError::InvalidLength);
        }
        let mut b = Vec::new();
        put_scope(&mut b, self.scope);
        b.extend(self.caller.0);
        put_id(&mut b, self.id.node, self.id.sequence);
        blob(&mut b, self.method.as_bytes());
        for v in [self.version, self.schema, self.handler] {
            b.extend(v.to_le_bytes());
        }
        frontier(&mut b, &self.frontier);
        b.extend(self.digest);
        if !self.arguments.is_empty() || self.policy != [0; 32] || self.client_request != [0; 32] {
            b.extend(2u32.to_le_bytes());
            blob(&mut b, &self.arguments);
            b.extend(self.policy);
            b.extend(self.client_request);
        }
        if b.len() > MAX_STRONG_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        Ok(b)
    }
    pub fn hash(&self) -> Result<Commitment, ProtocolError> {
        Ok(commitment("rocky.mvp.request.v1", &self.encode()?))
    }
    pub fn decode(b: &[u8]) -> Result<Self, ProtocolError> {
        if b.len() > MAX_STRONG_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        let mut r = Wire(b);
        let scope = r.scope()?;
        let caller = PrincipalId(r.array()?);
        let (node, sequence) = r.id()?;
        let method =
            String::from_utf8(r.blob()?.to_vec()).map_err(|_| ProtocolError::InvalidLength)?;
        let mut s = Self {
            scope,
            caller,
            id: RequestId { node, sequence },
            method,
            version: r.u32()?,
            schema: r.u32()?,
            handler: r.u32()?,
            client_request: [0; 32],
            arguments: Vec::new(),
            policy: [0; 32],
            frontier: r.frontier()?,
            digest: r.array()?,
        };
        if !r.0.is_empty() {
            if r.u32()? != 2 {
                return Err(ProtocolError::InvalidLength);
            }
            s.arguments = r.blob()?.to_vec();
            s.policy = r.array()?;
            s.client_request = r.array()?;
            if s.arguments.is_empty() && s.policy == [0; 32] && s.client_request == [0; 32] {
                return Err(ProtocolError::InvalidLength);
            }
        }
        r.end()?;
        s.encode()?;
        Ok(s)
    }
}
impl Confirmation {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        b.extend(self.caller.0);
        put_scope(&mut b, self.scope);
        b.extend(self.configuration);
        b.extend(self.executor.0);
        b.extend(self.epoch.to_le_bytes());
        put_id(&mut b, self.request.node, self.request.sequence);
        b.extend(self.request_hash);
        b.extend(self.expected_sequence.to_le_bytes());
        b.extend(self.expected_head);
        b.extend(self.schema.to_le_bytes());
        b.extend(self.handler.to_le_bytes());
        b.extend(self.result);
        b.extend(self.new_head);
        b
    }
    pub fn decode(b: &[u8]) -> Result<Self, ProtocolError> {
        if b.len() > MAX_STRONG_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        let mut r = Wire(b);
        let caller = PrincipalId(r.array()?);
        let scope = r.scope()?;
        let configuration = r.array()?;
        let executor = NodeId(r.array()?);
        let epoch = r.u64()?;
        let (node, sequence) = r.id()?;
        let s = Self {
            caller,
            scope,
            configuration,
            executor,
            epoch,
            request: RequestId { node, sequence },
            request_hash: r.array()?,
            expected_sequence: r.u64()?,
            expected_head: r.array()?,
            schema: r.u32()?,
            handler: r.u32()?,
            result: r.array()?,
            new_head: r.array()?,
        };
        r.end()?;
        Ok(s)
    }
}
impl StrongReceipt {
    pub fn encode(&self) -> Vec<u8> {
        let mut b = Vec::new();
        blob(&mut b, &self.confirmation.encode());
        b.extend(self.sequence.to_le_bytes());
        b
    }
    pub fn decode(b: &[u8]) -> Result<Self, ProtocolError> {
        if b.len() > MAX_STRONG_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        let mut r = Wire(b);
        let s = Self {
            confirmation: Confirmation::decode(r.blob()?)?,
            sequence: r.u64()?,
        };
        r.end()?;
        Ok(s)
    }
}
impl StrongPackage {
    pub fn encode(&self) -> Result<Vec<u8>, ProtocolError> {
        // A request submitted under an earlier generation keeps its signed
        // identity and runs under the one the coordinator activated since.
        if self.request.scope != self.base.scope
            || self.request.schema > self.base.schema
            || self.request.handler > self.base.handler
            || self.base.sequence >= MAX_COUNTER
            || self.sequence != self.base.sequence + 1
        {
            return Err(ProtocolError::InvalidDependencies);
        }
        if cut_size(&self.cut_base, &self.request.frontier)? > MAX_CUT_OPERATIONS as u64 {
            return Err(ProtocolError::InvalidLength);
        }
        if self.rows.len() > MAX_STRONG_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        let mut b = PACKAGE_MAGIC.to_vec();
        blob(&mut b, &self.request.encode()?);
        b.extend(self.base.configuration);
        b.extend(self.base.executor.0);
        b.extend(self.base.epoch.to_le_bytes());
        b.extend(self.base.sequence.to_le_bytes());
        b.extend(self.base.head);
        b.extend(self.base.schema.to_le_bytes());
        b.extend(self.base.handler.to_le_bytes());
        b.extend(self.sequence.to_le_bytes());
        frontier(&mut b, &self.request.frontier);
        frontier(&mut b, &self.cut_base);
        blob(&mut b, &self.rows);
        if b.len() > MAX_STRONG_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        Ok(b)
    }
    pub fn decode(b: &[u8]) -> Result<Self, ProtocolError> {
        if b.len() > MAX_STRONG_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        let mut r = Wire(b);
        if &r.array::<16>()? != PACKAGE_MAGIC {
            return Err(ProtocolError::InvalidLength);
        }
        let request = StrongRequest::decode(r.blob()?)?;
        let base = StrongHead {
            scope: request.scope,
            configuration: r.array()?,
            executor: NodeId(r.array()?),
            epoch: r.u64()?,
            sequence: r.u64()?,
            head: r.array()?,
            schema: r.u32()?,
            handler: r.u32()?,
        };
        let sequence = r.u64()?;
        if r.frontier()? != request.frontier {
            return Err(ProtocolError::InvalidDependencies);
        }
        let cut_base = r.frontier()?;
        let rows = r.blob()?.to_vec();
        r.end()?;
        let s = Self {
            request,
            base,
            sequence,
            rows,
            cut_base,
        };
        s.encode()?;
        Ok(s)
    }
    /// The operations the cut covers after its base: what a verifier must
    /// hold beyond its own checkpoint to rebuild the cut.
    pub fn required(&self) -> Result<Vec<OperationId>, ProtocolError> {
        cut_operations(&self.cut_base, &self.request.frontier)
    }
    /// The encoded request at the start of encoded package bytes, without
    /// decoding the rest.
    pub fn request_bytes(package: &[u8]) -> Option<&[u8]> {
        let rest = package.strip_prefix(PACKAGE_MAGIC)?;
        let (length, rest) = rest.split_first_chunk::<4>()?;
        rest.get(..u32::from_le_bytes(*length) as usize)
    }
    pub fn confirmation(&self) -> Result<Confirmation, ProtocolError> {
        let result = commitment("rocky.mvp.result.v1", &self.encode()?);
        let request_hash = self.request.hash()?;
        Ok(Confirmation {
            caller: self.request.caller,
            scope: self.request.scope,
            configuration: self.base.configuration,
            executor: self.base.executor,
            epoch: self.base.epoch,
            request: self.request.id,
            request_hash,
            expected_sequence: self.base.sequence,
            expected_head: self.base.head,
            schema: self.base.schema,
            handler: self.base.handler,
            result,
            new_head: successor(self.base.head, self.sequence, request_hash, result),
        })
    }
}
struct Wire<'a>(&'a [u8]);
impl<'a> Wire<'a> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], ProtocolError> {
        let (h, t) = self.0.split_at_checked(N).ok_or(ProtocolError::Truncated)?;
        self.0 = t;
        h.try_into().map_err(|_| ProtocolError::Truncated)
    }
    fn u32(&mut self) -> Result<u32, ProtocolError> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64, ProtocolError> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn blob(&mut self) -> Result<&'a [u8], ProtocolError> {
        let n = self.u32()? as usize;
        if n > MAX_STRONG_BYTES {
            return Err(ProtocolError::InvalidLength);
        }
        let (h, t) = self.0.split_at_checked(n).ok_or(ProtocolError::Truncated)?;
        self.0 = t;
        Ok(h)
    }
    fn scope(&mut self) -> Result<Scope, ProtocolError> {
        Ok(Scope {
            mesh: MeshId(self.array()?),
            app: AppId(self.array()?),
            domain: DomainId(self.array()?),
        })
    }
    fn id(&mut self) -> Result<(NodeId, AuthorSequence), ProtocolError> {
        Ok((NodeId(self.array()?), AuthorSequence::new(self.u64()?)?))
    }
    fn count(&mut self, size: usize) -> Result<usize, ProtocolError> {
        let n = self.u32()? as usize;
        if n > self.0.len() / size {
            return Err(ProtocolError::InvalidLength);
        }
        Ok(n)
    }
    fn frontier(&mut self) -> Result<AppliedFrontier, ProtocolError> {
        let n = self.count(24)?;
        let mut v = Vec::new();
        for _ in 0..n {
            v.push(self.id()?);
        }
        let f = AppliedFrontier(v);
        valid_frontier(&f)?;
        Ok(f)
    }
    fn end(&self) -> Result<(), ProtocolError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ProtocolError::TrailingBytes)
        }
    }
}
