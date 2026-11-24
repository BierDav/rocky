use crate::support::{
    public::{DAY, PublicCluster, transport},
    timestamp,
};
use rocky_coordinator::{
    CoordinationError, CounterError, Counters, Limits, Refusal,
    control::ControlReceipt,
    counters::{Claim, Lookup, Name},
    tenancy::{SignedTenantAdmission, Tenant, TenantAdmission},
};
use rocky_sdk::{
    Access, Affinity, Column, Configuration, Database, Network, PersonId, Removal, RootKey, Schema,
    Table, transport::memory::Hub,
};
use std::{path::Path, sync::Arc, time::Duration};

const MAX_CONTROL_LIFETIME: u64 = 30 * DAY;

fn configuration(directory: &Path, hub: &Arc<Hub>, name: &str) -> Configuration {
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
        .sync(rocky_sdk::SyncMode::Manual)
}
fn invoices() -> Name {
    Name::new("invoices").unwrap()
}

struct Office {
    directory: tempfile::TempDir,
    hub: Arc<Hub>,
    cluster: PublicCluster,
    publisher: Database,
    root: RootKey,
    tenant: Tenant,
}
impl Office {
    async fn open() -> Self {
        let cluster = PublicCluster::start(Limits::default(), 2).await;
        let directory = tempfile::tempdir().unwrap();
        let hub = Arc::new(Hub::default());
        let created = configuration(&directory.path().join("publisher"), &hub, "publisher")
            .create()
            .await
            .unwrap();
        let tenant = Tenant::generate();
        let admission = SignedTenantAdmission::sign(
            TenantAdmission {
                provider: cluster.provider.public_key(),
                tenant: tenant.control(),
                not_before: timestamp() - 60,
                expires: timestamp() + DAY,
            },
            &cluster.provider,
        )
        .unwrap();
        Self {
            directory,
            hub,
            cluster,
            publisher: created.database,
            root: created.root,
            tenant: tenant.admitted(admission).unwrap(),
        }
    }
    async fn person(&self, name: &str, access: Access) -> Database {
        let invitation = self
            .publisher
            .invite(rocky_sdk::Invite::new(access))
            .await
            .unwrap();
        configuration(&self.directory.path().join(name), &self.hub, name)
            .join(&invitation.code)
            .await
            .unwrap()
            .database
    }
    async fn counters(&self, database: &Database) -> Counters {
        Counters::connect(database.clone(), &self.cluster.bundle, transport().await).unwrap()
    }
    async fn registered(&self) -> (Counters, ControlReceipt) {
        let counters = self.counters(&self.publisher).await;
        let before = timestamp();
        let receipt = counters
            .register(&self.root, &self.tenant, MAX_CONTROL_LIFETIME)
            .await
            .unwrap();
        let not_before = receipt.expires - MAX_CONTROL_LIFETIME;
        assert!(
            (before - 60..=timestamp() - 60).contains(&not_before),
            "a control lives exactly the coordinator's longest lifetime"
        );
        (counters, receipt)
    }
    async fn restart_every_voter(&mut self) {
        let admin = self.cluster.admin();
        for id in 1..=3 {
            self.cluster.spawn(id, false);
        }
        for id in 1..=3 {
            self.cluster.wait_online(id, &admin).await;
        }
        self.cluster.wait_unfenced_leader(&admin).await;
    }
}

fn refused<T: std::fmt::Debug>(result: Result<T, CounterError>) -> Refusal {
    match result {
        Err(CounterError::Coordination(CoordinationError::Denied(refusal))) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}
fn unavailable<T>(result: Result<T, CounterError>) -> bool {
    matches!(
        result,
        Err(CounterError::Coordination(CoordinationError::Unavailable))
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_of_a_database_claim_gap_free_numbers_from_the_voters_and_a_keyed_retry_keeps_its_first()
 {
    let office = Office::open().await;
    let (mine, receipt) = office.registered().await;
    let other = office.person("other", Access::Full).await;
    let theirs = office.counters(&other).await;
    let issued = timestamp();
    let orders = [
        (&mine, Claim::keyed(b"order-a1", issued)),
        (&theirs, Claim::keyed(b"order-b1", issued)),
        (&mine, Claim::keyed(b"order-a2", issued)),
        (&theirs, Claim::keyed(b"order-b2", issued)),
    ];
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
    assert_eq!(theirs.read(&invoices(), None).await.unwrap().value, 4);

    assert_eq!(
        refused(
            mine.register(&office.root, &office.tenant, MAX_CONTROL_LIFETIME - 1)
                .await
        ),
        Refusal::AlreadyRegistered,
        "a registered domain refuses another first control"
    );
    let renewed = mine
        .renew(&office.tenant, &receipt, MAX_CONTROL_LIFETIME)
        .await
        .unwrap();
    assert_eq!(renewed.revision, receipt.revision + 1);
    assert_eq!(
        theirs
            .claim(&invoices(), Claim::keyed(b"order-b3", issued))
            .await
            .unwrap(),
        5,
        "a renewed control keeps the counters"
    );
    assert!(
        matches!(
            mine.register(&office.root, &office.tenant, MAX_CONTROL_LIFETIME + 1)
                .await,
            Err(CounterError::Coordination(CoordinationError::Configuration))
        ),
        "a lifetime beyond the coordinator's longest is refused before it is sent"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_and_a_removed_person_are_refused_with_typed_refusals() {
    let office = Office::open().await;
    let (publisher, _) = office.registered().await;
    let writer = office.person("writer", Access::Full).await;
    let reader = office.person("reader", Access::Read).await;
    let (writing, reading) = (
        office.counters(&writer).await,
        office.counters(&reader).await,
    );
    let issued = timestamp();
    assert_eq!(
        writing
            .claim(&invoices(), Claim::keyed(b"order-1", issued))
            .await
            .unwrap(),
        1
    );
    assert_eq!(
        refused(
            reading
                .claim(&invoices(), Claim::keyed(b"order-2", issued))
                .await
        ),
        Refusal::ReadOnly
    );
    assert_eq!(
        reading.read(&invoices(), None).await.unwrap().value,
        1,
        "a reader still reads the counter"
    );

    let removed = writer.device_credential().await.unwrap().identity;
    office
        .publisher
        .remove(PersonId::new(removed), Removal::Departed)
        .await
        .unwrap();
    publisher.share_revocations().await.unwrap();
    assert_eq!(
        refused(
            writing
                .claim(&invoices(), Claim::keyed(b"order-3", issued))
                .await
        ),
        Refusal::Revoked
    );
    assert_eq!(
        refused(
            writing
                .claim(&invoices(), Claim::keyed(b"order-1", issued))
                .await
        ),
        Refusal::Revoked,
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_unreachable_coordinator_is_unavailable_and_takes_no_number() {
    let mut office = Office::open().await;
    let counters = office
        .registered()
        .await
        .0
        .with_deadline(Duration::from_secs(2));
    let claim = Claim::keyed(b"order-1", timestamp());
    for id in 1..=3 {
        office.cluster.stop(id);
    }
    assert!(unavailable(counters.claim(&invoices(), claim).await));
    assert!(unavailable(counters.read(&invoices(), Some(claim)).await));

    office.restart_every_voter().await;
    let counters = counters.with_deadline(rocky_coordinator::service::CALL_DEADLINE);
    let reading = counters.read(&invoices(), Some(claim)).await.unwrap();
    assert_eq!(
        (reading.value, reading.claim),
        (0, Some(Lookup::Unclaimed)),
        "a claim the coordinator never answered took no number"
    );
    assert_eq!(counters.claim(&invoices(), claim).await.unwrap(), 1);
    assert_eq!(
        counters.claim(&invoices(), claim).await.unwrap(),
        1,
        "the keyed retry returns the first number"
    );
}
