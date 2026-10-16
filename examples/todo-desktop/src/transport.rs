//! Bounded development transport. All authoritative progress stays in the replicas.
use rocky_core::{Envelope, NodeId, StoredApplication, fixtures};
use rocky_runtime::{Acknowledgment, Catchup, Error as RuntimeError, Replica};
use std::collections::VecDeque;

#[derive(Debug)]
pub enum Error {
    QueueFull,
    InvalidControl(String),
    Rejected(RuntimeError),
    Runtime(RuntimeError),
}
impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::QueueFull => write!(f, "transport queue is full; retry after delivery"),
            Self::InvalidControl(message) => write!(f, "transport control: {message}"),
            Self::Rejected(error) => write!(f, "replication rejected data: {error}"),
            Self::Runtime(error) => write!(f, "replication storage/delivery failed: {error}"),
        }
    }
}
impl std::error::Error for Error {}
impl From<RuntimeError> for Error {
    fn from(error: RuntimeError) -> Self {
        match error {
            error @ (RuntimeError::Protocol(_)
            | RuntimeError::Application(_)
            | RuntimeError::Rejected(_)) => Self::Rejected(error),
            error => Self::Runtime(error),
        }
    }
}

/// Copies `from`'s latest checkpoint into `to`, which is behind `from`'s
/// pruned prefix. The development adapter moves pages directly, not through its
/// queue. Local operations of `to` are kept and applied again.
pub async fn rebootstrap<A>(from: &Replica<A>, to: &Replica<A>) -> Result<(), Error>
where
    A: StoredApplication,
    A::State: Clone,
    A::Policy: Send + 'static,
{
    let mut cursor = None;
    loop {
        let page = from.checkpoint_page(cursor).await?;
        cursor = page.next;
        if to.import_checkpoint(page).await?.is_some() || cursor.is_none() {
            return Ok(());
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Message {
    Operation(Vec<u8>),
    Acknowledgment(Acknowledgment),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Delivery {
    pub to: usize,
    pub message: Message,
}

pub struct SimulatedTransport {
    queue: VecDeque<Delivery>,
    capacity: usize,
    paused: bool,
    next_direction: usize,
}
impl SimulatedTransport {
    pub fn new(capacity: usize) -> Result<Self, Error> {
        if capacity == 0 {
            return Err(Error::InvalidControl(
                "queue capacity must be positive".into(),
            ));
        }
        Ok(Self {
            queue: VecDeque::new(),
            capacity,
            paused: false,
            next_direction: 0,
        })
    }
    pub fn set_paused(&mut self, paused: bool) {
        self.paused = paused;
    }
    pub fn queued(&self) -> &VecDeque<Delivery> {
        &self.queue
    }
    pub fn enqueue(&mut self, delivery: Delivery) -> Result<(), Error> {
        if delivery.to > 1 {
            return Err(Error::InvalidControl(
                "invalid transport destination".into(),
            ));
        }
        if self.queue.len() >= self.capacity {
            return Err(Error::QueueFull);
        }
        self.queue.push_back(delivery);
        Ok(())
    }
    pub fn drop_message(&mut self, index: usize) -> Option<Delivery> {
        self.queue.remove(index)
    }
    pub fn clear(&mut self) {
        self.queue.clear();
    }
    pub fn duplicate(&mut self, index: usize) -> Result<(), Error> {
        let message = self
            .queue
            .get(index)
            .cloned()
            .ok_or_else(|| Error::InvalidControl("invalid queue index".into()))?;
        self.enqueue(message)
    }
    /// Reconcile exact durable receipt sets, including pending dependencies.
    /// Returns with a full queue; the caller delivers and retries to make progress.
    pub async fn reconcile<A>(&mut self, replicas: &[Replica<A>; 2]) -> Result<(), Error>
    where
        A: StoredApplication,
        A::State: Clone,
        A::Policy: Send + 'static,
    {
        if self.paused {
            return Ok(());
        }
        for offset in 0..2 {
            let from = (self.next_direction + offset) % 2;
            let to = 1 - from;
            let source = replicas[from].sync_state().await?;
            let target = replicas[to].sync_state().await?;
            // The source may have pruned what the target lacks; then the
            // target takes the source's checkpoint and syncs from there.
            let page = match replicas[from]
                .catch_up(Some(node(to)), target.clone())
                .await?
            {
                Catchup::Operations(page) => page,
                Catchup::BehindPruned(_) => {
                    rebootstrap(&replicas[from], &replicas[to]).await?;
                    continue;
                }
            };
            // Repair lost received/applied acknowledgments independently of the outbox.
            let acks = replicas[from].peer_acknowledgments(node(to)).await?;
            for id in target
                .received
                .iter()
                .filter(|id| source.received.binary_search(id).is_ok())
            {
                let applied = target
                    .applied
                    .0
                    .iter()
                    .any(|(author, sequence)| *author == id.node && *sequence >= id.sequence);
                if acks.iter().any(|a| a.id == *id && (a.applied || !applied)) {
                    continue;
                }
                let delivery = Delivery {
                    to: from,
                    message: Message::Acknowledgment(Acknowledgment { id: *id, applied }),
                };
                if !self.queue.contains(&delivery) {
                    if self.queue.len() == self.capacity {
                        self.next_direction = to;
                        return Ok(());
                    }
                    self.enqueue(delivery)?;
                }
            }
            for bytes in page.records {
                let delivery = Delivery {
                    to,
                    message: Message::Operation(bytes),
                };
                if !self.queue.contains(&delivery) {
                    if self.queue.len() == self.capacity {
                        self.next_direction = to;
                        return Ok(());
                    }
                    self.enqueue(delivery)?;
                }
            }
        }
        self.next_direction = 1 - self.next_direction;
        Ok(())
    }
    /// Arbitrary indices allow deterministic delay and reordering. Failed storage
    /// delivery stays queued; rejected data is removed and reported to the caller.
    pub async fn deliver<A>(
        &mut self,
        index: usize,
        replicas: &[Replica<A>; 2],
    ) -> Result<bool, Error>
    where
        A: StoredApplication,
        A::State: Clone,
        A::Policy: Send + 'static,
    {
        if self.paused {
            return Ok(false);
        }
        let Some(delivery) = self.queue.get(index).cloned() else {
            return Ok(false);
        };
        let receiver = &replicas[delivery.to];
        let result = match delivery.message {
            Message::Operation(bytes) => receiver.receive(bytes).await.map(|_| ()),
            Message::Acknowledgment(ack) => receiver.acknowledge(node(1 - delivery.to), ack).await,
        };
        match result {
            Ok(()) => {
                self.queue.remove(index);
            }
            Err(
                error @ (RuntimeError::Protocol(_)
                | RuntimeError::Application(_)
                | RuntimeError::Rejected(_)),
            ) => {
                self.queue.remove(index);
                return Err(error.into());
            }
            Err(error) => return Err(error.into()),
        }
        receiver.process_pending().await?;
        Ok(true)
    }
    /// One bounded UI pump. Later pumps recover queue loss from retained history.
    pub async fn pump<A>(&mut self, replicas: &[Replica<A>; 2]) -> Result<(), Error>
    where
        A: StoredApplication,
        A::State: Clone,
        A::Policy: Send + 'static,
    {
        for replica in replicas {
            replica.process_pending().await?;
        }
        self.reconcile(replicas).await?;
        for _ in 0..self.capacity {
            let Some(index) = self.queue.iter().position(|d| {
                matches!(
                    d.message,
                    Message::Operation(_) | Message::Acknowledgment(_)
                )
            }) else {
                break;
            };
            if !self.deliver(index, replicas).await? {
                break;
            }
        }
        Ok(())
    }
}
fn node(index: usize) -> NodeId {
    if index == 0 {
        fixtures::LAPTOP
    } else {
        fixtures::PHONE
    }
}

/// Decode only for harness inspection; transfer always preserves original bytes.
pub fn operation(delivery: &Delivery) -> Result<Option<Envelope>, Error> {
    match &delivery.message {
        Message::Operation(bytes) => Ok(Some(Envelope::decode(bytes).map_err(RuntimeError::from)?)),
        _ => Ok(None),
    }
}

impl Default for SimulatedTransport {
    fn default() -> Self {
        Self {
            queue: VecDeque::new(),
            capacity: 32,
            paused: false,
            next_direction: 0,
        }
    }
}
