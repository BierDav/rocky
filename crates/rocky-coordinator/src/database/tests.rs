use super::*;
use crate::{
    client::tests::{CallerOwned, KIND, Lane, Service},
    counters::Lookup,
};
use p2panda_core::PrivateKey;
use rocky_sdk::{
    Access, Affinity, Column, Configuration, Network, Schema, SqlValue, Table,
    transport::memory::Hub,
};
use std::path::Path;

fn open(directory: &Path, hub: &Arc<Hub>, name: &str) -> Configuration {
    let schema = Schema::builder("billing", 1)
        .table(
            Table::replayed("invoices")
                .column(Column::new("key", Affinity::Text).required())
                .column(Column::new("number", Affinity::Integer).required())
                .primary_key(["key"]),
        )
        .build()
        .unwrap();
    Configuration::new(directory, &schema)
        .unwrap()
        .network(Network::Owned(hub.connector(name)))
}
fn invoices() -> Name {
    Name::new("invoices").unwrap()
}

struct Office {
    _directory: tempfile::TempDir,
    hub: Arc<Hub>,
    service: Service,
    transport: Arc<CallerOwned>,
    publisher: Database,
}
impl Office {
    async fn open() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let hub = Arc::new(Hub::default());
        let created = open(&directory.path().join("publisher"), &hub, "publisher")
            .create()
            .await
            .unwrap();
        let (database, root) = (created.database, created.root);
        let service = Service::serving(None, &root, database.scope(), "billing").await;
        let transport = CallerOwned::new(Lane(service.voter.clone(), KIND));
        Self {
            _directory: directory,
            hub,
            service,
            transport,
            publisher: database,
        }
    }
    async fn person(&self, name: &str, access: Access) -> Database {
        let invitation = self
            .publisher
            .invite(rocky_sdk::Invite::new(access))
            .await
            .unwrap();
        open(&self._directory.path().join(name), &self.hub, name)
            .join(&invitation.code)
            .await
            .unwrap()
            .database
    }
    async fn writer(&self, name: &str) -> Database {
        self.person(name, Access::Full).await
    }
    fn counters(&self, database: &Database) -> Counters {
        let client = self
            .service
            .client_on(&PrivateKey::new(), self.transport.clone());
        Counters::new(database.clone(), Arc::new(client))
    }
    fn offline(&self, offline: bool) {
        self.transport.refuse_connections(offline);
    }
}

fn denied(result: Result<u64, CounterError>) -> bool {
    matches!(
        result,
        Err(CounterError::Coordination(CoordinationError::Denied))
    )
}
fn unavailable<T>(result: Result<T, CounterError>) -> bool {
    matches!(
        result,
        Err(CounterError::Coordination(CoordinationError::Unavailable))
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn two_devices_claim_offline_then_online_and_get_gap_free_increasing_numbers() {
    let office = Office::open().await;
    let other = office.writer("other").await;
    let (mine, theirs) = (office.counters(&office.publisher), office.counters(&other));
    let issued = now().unwrap();
    let orders = [
        (&mine, Claim::keyed(b"order-a1", issued)),
        (&theirs, Claim::keyed(b"order-b1", issued)),
        (&mine, Claim::keyed(b"order-a2", issued)),
        (&theirs, Claim::keyed(b"order-b2", issued)),
    ];
    office.offline(true);
    for (counters, claim) in &orders {
        assert!(unavailable(counters.claim(&invoices(), *claim).await));
    }
    office.offline(false);
    let mut numbers = Vec::new();
    for (counters, claim) in &orders {
        numbers.push(counters.claim(&invoices(), *claim).await.unwrap());
    }
    assert_eq!(numbers, [1, 2, 3, 4], "gap-free and strictly increasing");
    for ((counters, claim), number) in orders.iter().zip(&numbers) {
        assert_eq!(
            counters.claim(&invoices(), *claim).await.unwrap(),
            *number,
            "a retry under the same key answers its first number"
        );
    }
    assert_eq!(mine.read(&invoices(), None).await.unwrap().value, 4);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unreachable_coordinator_gives_a_typed_error_and_takes_no_number() {
    let office = Office::open().await;
    let counters = office.counters(&office.publisher);
    let claim = Claim::keyed(b"order-1", now().unwrap());
    office.offline(true);
    assert!(unavailable(counters.claim(&invoices(), claim).await));
    assert!(unavailable(counters.read(&invoices(), Some(claim)).await));
    office.offline(false);
    let reading = counters.read(&invoices(), Some(claim)).await.unwrap();
    assert_eq!(
        (reading.value, reading.claim),
        (0, Some(Lookup::Unclaimed)),
        "a refused claim took no number"
    );
    assert_eq!(counters.claim(&invoices(), claim).await.unwrap(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_removed_person_or_a_reader_never_claims() {
    let office = Office::open().await;
    let writer = office.writer("writer").await;
    let reader = office.person("reader", Access::Read).await;
    let (publisher, writing, reading) = (
        office.counters(&office.publisher),
        office.counters(&writer),
        office.counters(&reader),
    );
    let issued = now().unwrap();
    assert_eq!(
        writing
            .claim(&invoices(), Claim::keyed(b"order-1", issued))
            .await
            .unwrap(),
        1
    );
    assert!(denied(
        reading
            .claim(&invoices(), Claim::keyed(b"order-2", issued))
            .await
    ));
    assert_eq!(
        reading.read(&invoices(), None).await.unwrap().value,
        1,
        "a reader still reads the counter"
    );

    let removed = writer.device_credential().await.unwrap().identity;
    office
        .publisher
        .remove(
            rocky_sdk::PersonId::new(removed),
            rocky_sdk::Removal::Departed,
        )
        .await
        .unwrap();
    publisher.share_revocations().await.unwrap();
    assert!(denied(
        writing
            .claim(&invoices(), Claim::keyed(b"order-3", issued))
            .await
    ));
    assert!(
        denied(
            writing
                .claim(&invoices(), Claim::keyed(b"order-1", issued))
                .await
        ),
        "a removed person cannot even replay an earlier claim"
    );
    assert_eq!(
        publisher
            .claim(&invoices(), Claim::keyed(b"order-4", issued))
            .await
            .unwrap(),
        2
    );
}

async fn record(database: &Database, key: &str, number: u64, commit: bool) {
    let mut transaction = database.transaction().await.unwrap();
    let id = transaction.next_id("invoices", Vec::new()).await.unwrap();
    transaction
        .execute(
            "INSERT INTO invoices(id, key, number) VALUES (?, ?, ?)",
            vec![
                id.sql(),
                SqlValue::Text(key.into()),
                SqlValue::Integer(i64::try_from(number).unwrap()),
            ],
        )
        .await
        .unwrap();
    if commit {
        transaction.commit().await.unwrap();
    } else {
        transaction.rollback().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_claimed_number_is_written_once_through_a_retried_transaction() {
    let office = Office::open().await;
    let database = &office.publisher;
    let counters = office.counters(database);
    let first = Claim::keyed(b"order-17", now().unwrap());

    let number = counters.claim(&invoices(), first).await.unwrap();
    record(database, "order-17", number, false).await;
    let retried = counters.claim(&invoices(), first).await.unwrap();
    assert_eq!(
        retried, number,
        "the stable claim key answers the same number"
    );
    record(database, "order-17", retried, true).await;

    let next = Claim::keyed(b"order-18", now().unwrap());
    let following = counters.claim(&invoices(), next).await.unwrap();
    record(database, "order-18", following, true).await;

    let rows = database
        .query(
            "SELECT key, number FROM invoices ORDER BY number",
            Vec::new(),
        )
        .await
        .unwrap()
        .collect()
        .await
        .unwrap();
    let stored: Vec<(String, i64)> = rows
        .iter()
        .map(|row| (row.get("key").unwrap(), row.get("number").unwrap()))
        .collect();
    assert_eq!(
        stored,
        [("order-17".to_owned(), 1), ("order-18".to_owned(), 2)],
        "the abandoned attempt left no row and no gap"
    );
}
