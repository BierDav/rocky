//! Envelope delivery between two admitted peers: the dialer hands over what
//! it holds for the acceptor, deposits envelopes for others with a mailbox,
//! then collects what the acceptor holds for it and returns receipts. The
//! session carries opaque sealed deliveries; their owner implements
//! `Deliveries`. See `docs/decisions/0054-envelope-delivery.md`.
use super::*;
use rocky_credentials::{DeliveryReceipt, SealedDelivery};
use std::{future::Future, pin::Pin};

/// Bytes of deliveries one message carries: the session's array encoding of
/// message bytes doubles them, inside one 256 KiB frame.
pub const DELIVERY_PAGE: usize = 96 * 1024;
/// Deliveries or receipts one message carries.
const MAX_ITEMS: usize = 256;
/// Collect rounds one session runs.
const MAX_ROUNDS: usize = 64;
const MAX_REFUSAL: usize = 256;

pub type DeliveryFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

/// What the dialer hands one peer: deliveries addressed to that peer, and
/// deposits for other recipients when the peer is a mailbox.
#[derive(Default)]
pub struct Outgoing {
    pub direct: Vec<SealedDelivery>,
    pub deposits: Vec<SealedDelivery>,
}
/// What one side holds for a collecting peer: deliveries addressed to it,
/// and the receipts of deposits it made.
#[derive(Default)]
pub struct Held {
    pub deliveries: Vec<SealedDelivery>,
    pub receipts: Vec<DeliveryReceipt>,
    /// More is held than fit this page.
    pub more: bool,
}
/// What one delivery round moved, as the dialer saw it.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delivered {
    /// Deliveries the peer acknowledged.
    pub handed: usize,
    /// Deposits the mailbox stored.
    pub deposited: usize,
    /// Deposits the mailbox refused; the sender keeps them.
    pub refused: usize,
    /// Deliveries received from the peer.
    pub received: usize,
    /// Receipts of this side's deposits the mailbox returned.
    pub receipts: usize,
}

/// The owner of delivery state: a sender's pending envelopes, a recipient's
/// installation and a mailbox's deposits. Every call names the session's
/// authenticated peer; the owner decides what that peer may hand or take.
pub trait Deliveries: Send + Sync {
    /// What this side hands `peer`, whose verified chain is `chain`.
    fn outgoing<'a>(
        &'a self,
        peer: PublicKey,
        chain: &'a CredentialChain,
    ) -> DeliveryFuture<'a, Outgoing>;
    /// Installs what `peer` handed over, returning a receipt for each
    /// delivery now held here, installed before or not.
    fn receive(
        &self,
        peer: PublicKey,
        deliveries: Vec<SealedDelivery>,
    ) -> DeliveryFuture<'_, Vec<DeliveryReceipt>>;
    /// Holds `peer`'s deposits for other recipients, as a mailbox: a
    /// refusal for each one not stored.
    fn deposit<'a>(
        &'a self,
        peer: PublicKey,
        chain: &'a CredentialChain,
        deposits: Vec<SealedDelivery>,
    ) -> DeliveryFuture<'a, Vec<Option<String>>>;
    /// The mailbox `peer` stored these deposits.
    fn deposited(&self, peer: PublicKey, stored: Vec<Commitment>) -> DeliveryFuture<'_, ()>;
    /// What this side holds for `peer`, at most about `budget` bytes.
    fn held(&self, peer: PublicKey, budget: usize) -> DeliveryFuture<'_, Held>;
    /// Receipts `peer` returned, signed by each recipient: for deliveries
    /// this side handed over, or deposits it holds as a mailbox.
    fn acknowledged(
        &self,
        peer: PublicKey,
        receipts: Vec<DeliveryReceipt>,
    ) -> DeliveryFuture<'_, ()>;
    /// `peer` keeps the receipts of its deposits: the mailbox forgets them.
    fn forget(&self, peer: PublicKey, digests: Vec<Commitment>) -> DeliveryFuture<'_, ()>;
}

#[derive(Serialize, Deserialize)]
pub(super) enum DeliveryMessage {
    /// Deliveries addressed to the receiving side.
    Hand {
        deliveries: Vec<Blob>,
    },
    Receipts {
        receipts: Vec<Blob>,
    },
    /// Deliveries for other recipients, for a mailbox to hold.
    Deposit {
        deliveries: Vec<Blob>,
    },
    Deposited {
        refusals: Vec<Option<String>>,
    },
    /// Asks what the receiving side holds for the sender.
    Collect,
    Collected {
        deliveries: Vec<Blob>,
        receipts: Vec<Blob>,
        more: bool,
    },
    /// Receipts of collected deliveries, and the digests of collected
    /// receipts the collector now keeps.
    Taken {
        receipts: Vec<Blob>,
        #[serde(with = "serde_bytes")]
        forgotten: Vec<u8>,
    },
}
#[derive(Serialize, Deserialize)]
pub(super) struct Blob(#[serde(with = "serde_bytes")] Vec<u8>);

fn blobs<T>(
    items: &[T],
    bytes: impl Fn(&T) -> Result<Vec<u8>, rocky_credentials::Error>,
) -> Result<Vec<Blob>, Error> {
    items
        .iter()
        .map(|item| bytes(item).map(Blob).map_err(credential))
        .collect()
}
fn deliveries(blobs: Vec<Blob>) -> Result<Vec<SealedDelivery>, Error> {
    if blobs.len() > MAX_ITEMS {
        return Err(denied("deliveries outside the exchange"));
    }
    blobs
        .into_iter()
        .map(|Blob(bytes)| SealedDelivery::from_bytes(&bytes).map_err(credential))
        .collect()
}
fn receipts(blobs: Vec<Blob>) -> Result<Vec<DeliveryReceipt>, Error> {
    if blobs.len() > MAX_ITEMS {
        return Err(denied("receipts outside the exchange"));
    }
    blobs
        .into_iter()
        .map(|Blob(bytes)| DeliveryReceipt::from_bytes(&bytes).map_err(credential))
        .collect()
}
/// Pages of at most `DELIVERY_PAGE` bytes and `MAX_ITEMS` deliveries; a
/// larger delivery travels alone.
fn pages(items: Vec<SealedDelivery>) -> Result<Vec<Vec<SealedDelivery>>, Error> {
    let mut pages: Vec<Vec<SealedDelivery>> = Vec::new();
    let mut bytes = 0;
    for item in items {
        let size = item.payload_len();
        match pages.last_mut() {
            Some(page) if page.len() < MAX_ITEMS && bytes + size <= DELIVERY_PAGE => {
                bytes += size;
                page.push(item);
            }
            _ => {
                bytes = size;
                pages.push(vec![item]);
            }
        }
    }
    Ok(pages)
}

impl NativeSession {
    /// Serves every family until the peer ends the session: deliveries
    /// from their owner, and contact metadata through `handler`.
    pub async fn serve_all<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        deliveries: &dyn Deliveries,
        handler: Option<&dyn ContactHandler>,
    ) -> Result<(), Error> {
        loop {
            let message = match self.answered_request().await {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                other => other?,
            };
            match (message, handler) {
                (Message::Contacts(message), Some(handler)) => {
                    self.answer_contact(replica, handler, message).await?;
                }
                (Message::Delivery(message), _) => {
                    self.answer_delivery(deliveries, message).await?;
                }
                (message, _) => self.answer(replica, message).await?,
            }
        }
    }
    /// One delivery round with the acceptor: hand over, deposit, collect.
    pub async fn deliver(&mut self, port: &dyn Deliveries) -> Result<Delivered, Error> {
        let mut report = Delivered::default();
        let outgoing = port.outgoing(self.peer, &self.credentials).await?;
        for page in pages(outgoing.direct)? {
            let digests: Vec<Commitment> = page
                .iter()
                .map(SealedDelivery::digest)
                .collect::<Result<_, _>>()
                .map_err(credential)?;
            self.send(DeliveryMessage::Hand {
                deliveries: blobs(&page, SealedDelivery::to_bytes)?,
            })
            .await?;
            let DeliveryMessage::Receipts { receipts: answer } = self.reply().await? else {
                return Err(denied("unexpected delivery answer"));
            };
            let answer = receipts(answer)?;
            if answer
                .iter()
                .any(|receipt| !digests.contains(&receipt.digest()))
            {
                return Err(denied("receipts outside the exchange"));
            }
            report.handed += answer.len();
            port.acknowledged(self.peer, answer).await?;
        }
        for page in pages(outgoing.deposits)? {
            self.send(DeliveryMessage::Deposit {
                deliveries: blobs(&page, SealedDelivery::to_bytes)?,
            })
            .await?;
            let DeliveryMessage::Deposited { refusals } = self.reply().await? else {
                return Err(denied("unexpected deposit answer"));
            };
            if refusals.len() != page.len() {
                return Err(denied("deposit answer outside the exchange"));
            }
            let mut stored = Vec::new();
            for (deposit, refusal) in page.iter().zip(&refusals) {
                match refusal {
                    None => stored.push(deposit.digest().map_err(credential)?),
                    Some(_) => report.refused += 1,
                }
            }
            report.deposited += stored.len();
            port.deposited(self.peer, stored).await?;
        }
        for _ in 0..MAX_ROUNDS {
            self.send(DeliveryMessage::Collect).await?;
            let DeliveryMessage::Collected {
                deliveries: collected,
                receipts: relayed,
                more,
            } = self.reply().await?
            else {
                return Err(denied("unexpected collect answer"));
            };
            let collected = deliveries(collected)?;
            let relayed = receipts(relayed)?;
            report.received += collected.len();
            report.receipts += relayed.len();
            let own = port.receive(self.peer, collected).await?;
            let forgotten: Vec<Commitment> = relayed.iter().map(DeliveryReceipt::digest).collect();
            port.acknowledged(self.peer, relayed).await?;
            let progress = !own.is_empty() || !forgotten.is_empty();
            self.send(DeliveryMessage::Taken {
                receipts: blobs(&own, DeliveryReceipt::to_bytes)?,
                forgotten: forgotten.concat(),
            })
            .await?;
            self.acknowledged("delivery receipts were not acknowledged")
                .await?;
            if !more || !progress {
                break;
            }
        }
        Ok(report)
    }
    async fn answer_delivery(
        &mut self,
        port: &dyn Deliveries,
        message: DeliveryMessage,
    ) -> Result<(), Error> {
        match message {
            DeliveryMessage::Hand { deliveries: handed } => {
                let receipts = port.receive(self.peer, deliveries(handed)?).await?;
                self.send(DeliveryMessage::Receipts {
                    receipts: blobs(&receipts, DeliveryReceipt::to_bytes)?,
                })
                .await
            }
            DeliveryMessage::Deposit {
                deliveries: deposits,
            } => {
                let refusals = port
                    .deposit(self.peer, &self.credentials, deliveries(deposits)?)
                    .await?
                    .into_iter()
                    .map(|refusal| refusal.map(|text| text.chars().take(MAX_REFUSAL).collect()))
                    .collect();
                self.send(DeliveryMessage::Deposited { refusals }).await
            }
            DeliveryMessage::Collect => {
                let held = port.held(self.peer, DELIVERY_PAGE).await?;
                self.send(DeliveryMessage::Collected {
                    deliveries: blobs(&held.deliveries, SealedDelivery::to_bytes)?,
                    receipts: blobs(&held.receipts, DeliveryReceipt::to_bytes)?,
                    more: held.more,
                })
                .await
            }
            DeliveryMessage::Taken {
                receipts: taken,
                forgotten,
            } => {
                if forgotten.len() % 32 != 0 || forgotten.len() / 32 > MAX_ITEMS {
                    return Err(denied("receipts outside the exchange"));
                }
                port.acknowledged(self.peer, receipts(taken)?).await?;
                port.forget(self.peer, forgotten.as_chunks::<32>().0.to_vec())
                    .await?;
                self.send(Message::Acknowledged).await
            }
            DeliveryMessage::Receipts { .. }
            | DeliveryMessage::Deposited { .. }
            | DeliveryMessage::Collected { .. } => Err(denied("unexpected delivery request")),
        }
    }
}
