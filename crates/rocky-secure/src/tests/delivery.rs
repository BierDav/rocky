//! The delivery family over a real session, against an in-memory owner.
use super::*;
use network::{Deliveries, DeliveryFuture, Held, NativeSession, Outgoing};
use rocky_credentials::{DeliveryReceipt, SealedDelivery};
use std::sync::{Arc, Mutex};

/// One side's delivery state: what it hands over and deposits, what it
/// holds for others, and every receipt and deposit it saw.
#[derive(Default)]
struct Port {
    key: Option<PrivateKey>,
    direct: Vec<SealedDelivery>,
    deposits: Vec<SealedDelivery>,
    held: Mutex<Vec<SealedDelivery>>,
    mailbox: Mutex<Vec<SealedDelivery>>,
    relayed: Mutex<Vec<DeliveryReceipt>>,
    received: Mutex<Vec<Vec<u8>>>,
    acknowledged: Mutex<Vec<Commitment>>,
    stored: Mutex<Vec<Commitment>>,
    forgotten: Mutex<Vec<Commitment>>,
    /// Deposits this mailbox refuses beyond.
    room: usize,
}
impl Deliveries for Port {
    fn outgoing<'a>(
        &'a self,
        _peer: PublicKey,
        _chain: &'a CredentialChain,
    ) -> DeliveryFuture<'a, Outgoing> {
        Box::pin(async {
            Ok(Outgoing {
                direct: self.direct.clone(),
                deposits: self.deposits.clone(),
            })
        })
    }
    fn receive(
        &self,
        _peer: PublicKey,
        deliveries: Vec<SealedDelivery>,
    ) -> DeliveryFuture<'_, Vec<DeliveryReceipt>> {
        Box::pin(async move {
            let key = self.key.as_ref().unwrap();
            let mut receipts = Vec::new();
            for delivery in deliveries {
                self.received
                    .lock()
                    .unwrap()
                    .push(delivery.open(key).unwrap());
                receipts.push(DeliveryReceipt::sign(key, delivery.digest().unwrap()).unwrap());
            }
            Ok(receipts)
        })
    }
    fn deposit<'a>(
        &'a self,
        _peer: PublicKey,
        _chain: &'a CredentialChain,
        deposits: Vec<SealedDelivery>,
    ) -> DeliveryFuture<'a, Vec<Option<String>>> {
        Box::pin(async move {
            let mut mailbox = self.mailbox.lock().unwrap();
            Ok(deposits
                .into_iter()
                .map(|deposit| {
                    if mailbox.len() >= self.room {
                        return Some("mailbox_full: retry later".into());
                    }
                    mailbox.push(deposit);
                    None
                })
                .collect())
        })
    }
    fn deposited(&self, _peer: PublicKey, stored: Vec<Commitment>) -> DeliveryFuture<'_, ()> {
        self.stored.lock().unwrap().extend(stored);
        Box::pin(async { Ok(()) })
    }
    fn held(&self, peer: PublicKey, budget: usize) -> DeliveryFuture<'_, Held> {
        Box::pin(async move {
            let mut deliveries = Vec::new();
            let mut bytes = 0;
            let held = self.held.lock().unwrap();
            for delivery in held.iter().filter(|delivery| delivery.recipient() == peer) {
                if bytes + delivery.payload_len() > budget && !deliveries.is_empty() {
                    return Ok(Held {
                        deliveries,
                        receipts: vec![],
                        more: true,
                    });
                }
                bytes += delivery.payload_len();
                deliveries.push(delivery.clone());
            }
            Ok(Held {
                deliveries,
                receipts: self.relayed.lock().unwrap().clone(),
                more: false,
            })
        })
    }
    fn acknowledged(
        &self,
        _peer: PublicKey,
        receipts: Vec<DeliveryReceipt>,
    ) -> DeliveryFuture<'_, ()> {
        let digests: Vec<Commitment> = receipts.iter().map(DeliveryReceipt::digest).collect();
        self.held
            .lock()
            .unwrap()
            .retain(|delivery| !digests.contains(&delivery.digest().unwrap()));
        self.acknowledged.lock().unwrap().extend(digests);
        Box::pin(async { Ok(()) })
    }
    fn forget(&self, _peer: PublicKey, digests: Vec<Commitment>) -> DeliveryFuture<'_, ()> {
        self.relayed
            .lock()
            .unwrap()
            .retain(|receipt| !digests.contains(&receipt.digest()));
        self.forgotten.lock().unwrap().extend(digests);
        Box::pin(async { Ok(()) })
    }
}

#[tokio::test]
async fn a_round_hands_over_deposits_and_collects_in_pages_with_receipts() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a, b, c) = (PrivateKey::new(), PrivateKey::new(), PrivateKey::new());
    let ac = fixture.config(&a, fixture.full(), &[fixture.public]);
    let bc = fixture.config(&b, fixture.full(), &[fixture.public]);
    let (acred, bcred) = (ac.credentials.clone(), bc.credentials.clone());
    let mailbox = EncryptedReplica::open(dir.path().join("a.db"), fixture.app(), ac)
        .await
        .unwrap();
    let sender = EncryptedReplica::open(dir.path().join("b.db"), fixture.app(), bc)
        .await
        .unwrap();
    // 40 KiB each: three pages travel each way.
    let large = |to: &PrivateKey, tag: u8| {
        SealedDelivery::seal(to.public_key(), &vec![tag; 40 * 1024]).unwrap()
    };
    let relayed = DeliveryReceipt::sign(&c, [7; 32]).unwrap();
    let mailbox_port = Arc::new(Port {
        key: Some(a.clone()),
        held: Mutex::new((0..5).map(|tag| large(&b, tag)).collect()),
        relayed: Mutex::new(vec![relayed.clone()]),
        room: 3,
        ..Port::default()
    });
    let sender_port = Port {
        key: Some(b.clone()),
        direct: (10..15).map(|tag| large(&a, tag)).collect(),
        deposits: (20..24).map(|tag| large(&c, tag)).collect(),
        ..Port::default()
    };
    let (dialer, acceptor) = tokio::io::duplex(1 << 20);
    let served = mailbox_port.clone();
    let server = async {
        let mut session =
            NativeSession::accept(acceptor, &mailbox, a.clone(), acred, MAX_SECURE_BYTES)
                .await
                .unwrap();
        session
            .serve_all(&mailbox, None, &*served, None)
            .await
            .unwrap();
    };
    let client = async {
        let mut session = NativeSession::authenticate(
            dialer,
            &sender,
            b.clone(),
            bcred,
            a.public_key(),
            MAX_SECURE_BYTES,
        )
        .await
        .unwrap();
        let report = session.deliver(&sender_port).await.unwrap();
        session.close().await.unwrap();
        report
    };
    let (_, report) = tokio::join!(server, client);
    assert_eq!(
        (
            report.handed,
            report.deposited,
            report.refused,
            report.received,
            report.receipts
        ),
        (5, 3, 1, 5, 1)
    );
    // Each side installed what was addressed to it, and acknowledged it.
    assert_eq!(mailbox_port.received.lock().unwrap().len(), 5);
    assert_eq!(sender_port.received.lock().unwrap().len(), 5);
    assert_eq!(sender_port.acknowledged.lock().unwrap().len(), 5 + 1);
    assert!(mailbox_port.held.lock().unwrap().is_empty());
    // The mailbox holds three deposits and forgot the receipt it relayed.
    assert_eq!(mailbox_port.mailbox.lock().unwrap().len(), 3);
    assert_eq!(sender_port.stored.lock().unwrap().len(), 3);
    assert_eq!(
        *mailbox_port.forgotten.lock().unwrap(),
        vec![relayed.digest()]
    );
}

#[tokio::test]
async fn a_session_without_an_owner_refuses_delivery() {
    let fixture = Fixture::new();
    let dir = TempDir::new().unwrap();
    let (a, b) = (PrivateKey::new(), PrivateKey::new());
    let ac = fixture.config(&a, fixture.full(), &[fixture.public]);
    let bc = fixture.config(&b, fixture.full(), &[fixture.public]);
    let (acred, bcred) = (ac.credentials.clone(), bc.credentials.clone());
    let served = EncryptedReplica::open(dir.path().join("a.db"), fixture.app(), ac)
        .await
        .unwrap();
    let dialing = EncryptedReplica::open(dir.path().join("b.db"), fixture.app(), bc)
        .await
        .unwrap();
    let (dialer, acceptor) = tokio::io::duplex(1 << 16);
    let server = async {
        let mut session =
            NativeSession::accept(acceptor, &served, a.clone(), acred, MAX_SECURE_BYTES)
                .await
                .unwrap();
        session.serve(&served).await
    };
    let client = async {
        let mut session = NativeSession::authenticate(
            dialer,
            &dialing,
            b,
            bcred,
            a.public_key(),
            MAX_SECURE_BYTES,
        )
        .await
        .unwrap();
        session.deliver(&Port::default()).await
    };
    let (served, delivered) = tokio::join!(server, client);
    assert!(
        served
            .unwrap_err()
            .to_string()
            .contains("serves no envelope delivery")
    );
    assert!(delivered.is_err());
}
