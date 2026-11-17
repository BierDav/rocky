use crate::{Command, Error, Replica};
use rocky_core::{AppliedFrontier, NodeId, OperationId, StoredApplication};
use tokio::sync::oneshot;

pub const TRANSFER_PAGE_SIZE: usize = 32;
pub const MAX_SYNC_EXCEPTIONS: usize = 256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncState {
    pub held: AppliedFrontier,
    pub received: Vec<OperationId>,
    pub applied: AppliedFrontier,
    pub pruned_prefix: AppliedFrontier,
}
impl SyncState {
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
    ReceivePage(Vec<Vec<u8>>),
    Process,
    State,
    Operation(OperationId),
    Page(Option<OperationId>),
    Missing(Vec<OperationId>),
    Acknowledge(NodeId, Acknowledgment),
    Progress(NodeId),
    PeerAcknowledgments(NodeId),
    PeerAcknowledgment(NodeId, Option<OperationId>),
}
impl Request {
    pub(crate) fn writes(&self) -> bool {
        matches!(
            self,
            Self::Receive(_) | Self::ReceivePage(_) | Self::Process | Self::Acknowledge(..)
        )
    }
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
    pub async fn receive(&self, bytes: Vec<u8>) -> Result<Acknowledgment, Error> {
        match self.sync_request(Request::Receive(bytes)).await? {
            Response::Ack(ack) => Ok(ack),
            _ => Err(Error::Corrupt("unexpected receipt response".into())),
        }
    }
    pub async fn receive_page(
        &self,
        records: Vec<Vec<u8>>,
    ) -> Result<Vec<Result<Acknowledgment, Error>>, Error> {
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
    pub async fn process_pending(&self) -> Result<(), Error> {
        loop {
            match self.sync_request(Request::Process).await? {
                Response::More(true) => rocky_platform::task::yield_now().await,
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
    pub(crate) async fn peer_acknowledgment(
        &self,
        peer: NodeId,
        id: Option<OperationId>,
    ) -> Result<Option<Acknowledgment>, Error> {
        match self
            .sync_request(Request::PeerAcknowledgment(peer, id))
            .await?
        {
            Response::Acks(acks) if acks.len() <= 1 => Ok(acks.into_iter().next()),
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
