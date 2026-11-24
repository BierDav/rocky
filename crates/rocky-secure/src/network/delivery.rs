use super::*;
use rocky_credentials::{DeliveryReceipt, SealedDelivery};
use std::{future::Future, pin::Pin};

pub const DELIVERY_PAGE: usize = 96 * 1024;
const MAX_ITEMS: usize = 256;
const MAX_ROUNDS: usize = 64;
const MAX_REFUSAL: usize = 256;

pub type DeliveryFuture<'a, T> = Pin<Box<dyn Future<Output = Result<T, Error>> + Send + 'a>>;

#[derive(Default)]
pub struct Outgoing {
    pub direct: Vec<SealedDelivery>,
    pub deposits: Vec<SealedDelivery>,
}
#[derive(Default)]
pub struct Held {
    pub deliveries: Vec<SealedDelivery>,
    pub receipts: Vec<DeliveryReceipt>,
    pub more: bool,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Delivered {
    pub handed: usize,
    pub deposited: usize,
    pub refused: usize,
    pub received: usize,
    pub receipts: usize,
}

pub trait Deliveries: Send + Sync {
    fn outgoing<'a>(
        &'a self,
        peer: PublicKey,
        chain: &'a CredentialChain,
    ) -> DeliveryFuture<'a, Outgoing>;
    fn receive(
        &self,
        peer: PublicKey,
        deliveries: Vec<SealedDelivery>,
    ) -> DeliveryFuture<'_, Vec<DeliveryReceipt>>;
    fn deposit<'a>(
        &'a self,
        peer: PublicKey,
        chain: &'a CredentialChain,
        deposits: Vec<SealedDelivery>,
    ) -> DeliveryFuture<'a, Vec<Option<String>>>;
    fn deposited(&self, peer: PublicKey, stored: Vec<Commitment>) -> DeliveryFuture<'_, ()>;
    fn held(&self, peer: PublicKey, budget: usize) -> DeliveryFuture<'_, Held>;
    fn acknowledged(
        &self,
        peer: PublicKey,
        receipts: Vec<DeliveryReceipt>,
    ) -> DeliveryFuture<'_, ()>;
    fn forget(&self, peer: PublicKey, digests: Vec<Commitment>) -> DeliveryFuture<'_, ()>;
    fn admits(&self, _peer: PublicKey) -> DeliveryFuture<'_, bool> {
        Box::pin(async { Ok(true) })
    }
}

#[derive(Serialize, Deserialize)]
pub(super) enum DeliveryMessage {
    Hand {
        deliveries: Vec<Blob>,
    },
    Receipts {
        receipts: Vec<Blob>,
    },
    Deposit {
        deliveries: Vec<Blob>,
    },
    Deposited {
        refusals: Vec<Option<String>>,
    },
    Collect,
    Collected {
        deliveries: Vec<Blob>,
        receipts: Vec<Blob>,
        more: bool,
    },
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
    pub async fn serve_all<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        deliveries: &dyn Deliveries,
        handler: Option<&dyn ContactHandler>,
    ) -> Result<(), Error> {
        if self.is_publication() {
            return self.receive_publication(replica).await.map(|_| ());
        }
        loop {
            let message = match self.answered_request().await {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                other => other?,
            };
            self.respond(replica, message, Some(deliveries), handler)
                .await?;
        }
    }
    pub async fn deliver(&mut self, port: &dyn Deliveries) -> Result<Delivered, Error> {
        let mut report = Delivered::default();
        let outgoing = port.outgoing(self.peer, &self.credentials).await?;
        for page in pages(outgoing.direct)? {
            if !port.admits(self.peer).await? {
                break;
            }
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
    pub(super) async fn answer_delivery(
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
                let held = if port.admits(self.peer).await? {
                    held
                } else {
                    Held::default()
                };
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

#[cfg(test)]
mod page_tests {
    use super::*;

    fn delivery(bytes: usize) -> SealedDelivery {
        SealedDelivery::seal(PrivateKey::new().public_key(), &vec![7; bytes]).unwrap()
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn delivery_pages_bound_bytes_and_items_and_an_oversized_delivery_travels_alone() {
        let small: Vec<_> = (0..MAX_ITEMS + 1).map(|_| delivery(1)).collect();
        let sizes: Vec<_> = pages(small).unwrap().iter().map(Vec::len).collect();
        assert_eq!(sizes, [MAX_ITEMS, 1]);

        let large = delivery(DELIVERY_PAGE + 1);
        let pages = pages(vec![delivery(1), large, delivery(1)]).unwrap();
        let sizes: Vec<_> = pages.iter().map(Vec::len).collect();
        assert_eq!(sizes, [1, 1, 1]);
        for page in &pages[..2] {
            let bytes: usize = page.iter().map(SealedDelivery::payload_len).sum();
            assert!(page.len() == 1 || bytes <= DELIVERY_PAGE);
        }
    }

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_full_delivery_page_fits_one_signed_frame() {
        let many_small = (0..MAX_ITEMS)
            .map(|_| delivery(DELIVERY_PAGE / MAX_ITEMS))
            .collect();
        let one_whole = vec![delivery(DELIVERY_PAGE)];
        for items in [many_small, one_whole] {
            let pages = pages(items).unwrap();
            assert_eq!(pages.len(), 1);
            for page in pages {
                let deliveries = blobs(&page, SealedDelivery::to_bytes).unwrap();
                crate::network::budgets::fits_one_frame(
                    DeliveryMessage::Hand { deliveries }.into(),
                );
                let deliveries = blobs(&page, SealedDelivery::to_bytes).unwrap();
                crate::network::budgets::fits_one_frame(
                    DeliveryMessage::Deposit { deliveries }.into(),
                );
            }
        }
    }
}
