use crate::{Command, Error, Replica};
use rocky_core::{AppliedFrontier, NodeId, OperationId, StoredApplication};
use tokio::sync::oneshot;

pub const TRANSFER_PAGE_SIZE: usize = 32;
/// Operations above the held frontier a `SyncState` lists.
pub const MAX_SYNC_EXCEPTIONS: usize = 256;

/// What a replica holds, in size independent of history: per-author
/// frontiers plus a bounded list of operations received out of order.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncState {
    /// Per author, the contiguous prefix this replica holds: behind its
    /// pruned prefix, applied, received or rejected.
    pub held: AppliedFrontier,
    /// Retained operations above `held`, including dependency-pending ones,
    /// lowest first. At most `MAX_SYNC_EXCEPTIONS`; a longer gap is resent.
    pub received: Vec<OperationId>,
    pub applied: AppliedFrontier,
    /// Everything at or before it is applied and no longer listed or sent.
    pub pruned_prefix: AppliedFrontier,
}
impl SyncState {
    /// Whether the replica holds `id`, as far as this state shows.
    pub fn holds(&self, id: OperationId) -> bool {
        self.held
            .0
            .iter()
            .any(|(node, sequence)| *node == id.node && id.sequence <= *sequence)
            || self.received.contains(&id)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Acknowledgment {
    pub id: OperationId,
    pub applied: bool,
}
#[derive(Clone, Debug)]
pub struct TransferPage {
    pub records: Vec<Vec<u8>>,
    /// Where a range request continues; `None` ends the pass.
    pub next: Option<OperationId>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PeerProgress {
    pub pending: usize,
    pub received: usize,
    pub applied: usize,
    pub frontier: AppliedFrontier,
}
pub(crate) enum Request {
    Receive(Vec<u8>),
    /// One bounded batch: receipt of each record and one application pass.
    ReceivePage(Vec<Vec<u8>>),
    Process,
    State,
    Operation(OperationId),
    Page(Option<OperationId>),
    Missing(Vec<OperationId>),
    Acknowledge(NodeId, Acknowledgment),
    Progress(NodeId),
    PeerAcknowledgments(NodeId),
}
impl Request {
    /// Receipt, application and acknowledgment write; the others only read.
    pub(crate) fn writes(&self) -> bool {
        matches!(
            self,
            Self::Receive(_) | Self::ReceivePage(_) | Self::Process | Self::Acknowledge(..)
        )
    }
    /// Replies a peer may see: operations, our receipt set and frontier, and
    /// the acknowledgment on which a sender drops its outbox entry. Recording
    /// a peer's acknowledgment stays local: if it is lost, we resend.
    pub(crate) fn exposes(&self) -> bool {
        matches!(
            self,
            Self::Receive(_)
                | Self::ReceivePage(_)
                | Self::State
                | Self::Operation(_)
                | Self::Page(_)
                | Self::Missing(_)
        )
    }
}
pub(crate) enum Response {
    Ack(Acknowledgment),
    Receipts(Vec<Result<Acknowledgment, Error>>),
    Unit,
    More(bool),
    State(SyncState),
    Operation(Option<Vec<u8>>),
    Operations(crate::Page<Vec<u8>, OperationId>),
    Page(TransferPage),
    Progress(PeerProgress),
    Acks(Vec<Acknowledgment>),
}
impl<A> Replica<A>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    async fn sync_request(&self, request: Request) -> Result<Response, Error> {
        let (reply, result) = oneshot::channel();
        self.inner
            .commands
            .send(Command::Sync(request, reply))
            .await?;
        result.await.map_err(|_| Error::Closed)?
    }
    pub async fn operation(
        &self,
        id: rocky_core::ScopedOperationId,
    ) -> Result<Option<Vec<u8>>, Error> {
        if id.scope != self.inner.scope {
            return Err(rocky_core::ProtocolError::ForeignScope.into());
        }
        match self.sync_request(Request::Operation(id.operation)).await? {
            Response::Operation(operation) => Ok(operation),
            _ => Err(Error::Corrupt("operation lookup".into())),
        }
    }
    /// Returns only after receipt commits. Applying dependencies is a separate step.
    pub async fn receive(&self, bytes: Vec<u8>) -> Result<Acknowledgment, Error> {
        match self.sync_request(Request::Receive(bytes)).await? {
            Response::Ack(ack) => Ok(ack),
            _ => Err(Error::Corrupt("unexpected receipt response".into())),
        }
    }
    /// Receives a page of operations in bounded batches of at most
    /// `TRANSFER_PAGE_SIZE` records and one frame of bytes. Each batch is one
    /// commit and one flush: every record is received in its own savepoint,
    /// then one application pass runs. The results are in input order. A
    /// refused record fails alone; an accepted one reports whether it is
    /// applied, and stays pending while a dependency is missing. An outer
    /// error means the batch it names did not commit; resending is safe.
    /// Call `process_pending` for work the single pass left.
    pub async fn receive_page(
        &self,
        records: Vec<Vec<u8>>,
    ) -> Result<Vec<Result<Acknowledgment, Error>>, Error> {
        // Room for the queue's fixed weight within one frame.
        const BATCH_BYTES: usize = crate::MAX_FRAME_BYTES - 1024;
        let mut results: Vec<Option<Result<Acknowledgment, Error>>> = Vec::new();
        let mut batch = Vec::new();
        let mut slots = Vec::new();
        let mut bytes = 0usize;
        let total = records.len();
        for (index, record) in records.into_iter().enumerate() {
            if record.len() > BATCH_BYTES {
                results.push(Some(Err(rocky_core::ProtocolError::InvalidLength.into())));
            } else {
                if batch.len() == TRANSFER_PAGE_SIZE || bytes + record.len() > BATCH_BYTES {
                    self.receive_batch(&mut batch, &mut slots, &mut results)
                        .await?;
                    bytes = 0;
                }
                bytes += record.len();
                slots.push(index);
                batch.push(record);
                results.push(None);
            }
            if index + 1 == total && !batch.is_empty() {
                self.receive_batch(&mut batch, &mut slots, &mut results)
                    .await?;
            }
        }
        results
            .into_iter()
            .map(|result| result.ok_or_else(|| Error::Corrupt("page receipt".into())))
            .collect()
    }
    async fn receive_batch(
        &self,
        batch: &mut Vec<Vec<u8>>,
        slots: &mut Vec<usize>,
        results: &mut [Option<Result<Acknowledgment, Error>>],
    ) -> Result<(), Error> {
        let request = Request::ReceivePage(std::mem::take(batch));
        let Response::Receipts(receipts) = self.sync_request(request).await? else {
            return Err(Error::Corrupt("unexpected page response".into()));
        };
        if receipts.len() != slots.len() {
            return Err(Error::Corrupt("page receipt count".into()));
        }
        for (slot, receipt) in slots.drain(..).zip(receipts) {
            results[slot] = Some(receipt);
        }
        Ok(())
    }
    /// Yields between bounded passes until no ready pending work remains.
    pub async fn process_pending(&self) -> Result<(), Error> {
        loop {
            match self.sync_request(Request::Process).await? {
                Response::More(true) => tokio::task::yield_now().await,
                Response::More(false) => return Ok(()),
                _ => return Err(Error::Corrupt("pending response".into())),
            }
        }
    }
    pub async fn operation_page(
        &self,
        after: Option<OperationId>,
    ) -> Result<crate::Page<Vec<u8>, OperationId>, Error> {
        match self.sync_request(Request::Page(after)).await? {
            Response::Operations(page) => Ok(page),
            _ => Err(Error::Corrupt("operation page".into())),
        }
    }
    pub async fn sync_state(&self) -> Result<SyncState, Error> {
        match self.sync_request(Request::State).await? {
            Response::State(state) => Ok(state),
            _ => Err(Error::Corrupt("unexpected state response".into())),
        }
    }
    pub async fn missing_operations(&self, known: Vec<OperationId>) -> Result<TransferPage, Error> {
        match self.sync_request(Request::Missing(known)).await? {
            Response::Page(page) => Ok(page),
            _ => Err(Error::Corrupt("unexpected transfer response".into())),
        }
    }
    pub async fn acknowledge(&self, peer: NodeId, ack: Acknowledgment) -> Result<(), Error> {
        self.sync_request(Request::Acknowledge(peer, ack))
            .await
            .map(|_| ())
    }
    pub async fn peer_acknowledgments(&self, peer: NodeId) -> Result<Vec<Acknowledgment>, Error> {
        match self
            .sync_request(Request::PeerAcknowledgments(peer))
            .await?
        {
            Response::Acks(acks) => Ok(acks),
            _ => Err(Error::Corrupt("unexpected acknowledgment response".into())),
        }
    }
    pub async fn peer_progress(&self, peer: NodeId) -> Result<PeerProgress, Error> {
        match self.sync_request(Request::Progress(peer)).await? {
            Response::Progress(progress) => Ok(progress),
            _ => Err(Error::Corrupt("unexpected progress response".into())),
        }
    }
}
