use crate::support::{
    ROW_CREATED,
    public::{DAY, PublicCluster},
    timestamp,
};
use rocky_coordinator::{
    Claim,
    Claimed::{First, Taken},
    ControlReceipt, CoordinationError, CounterError, Counters, Limits, Name, Refusal, Tenant,
    TrustBundle,
    tenancy::{SignedTenantAdmission, TenantAdmission},
};
use rocky_sdk::{
    Access, Affinity, Column, Configuration, Database, Network, PersonId, RootKey, Schema, Table,
    transport::memory::Hub,
};
use rocky_voter::LocalCluster;
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

enum Voters {
    InProcess(Box<LocalCluster>),
    Processes(Box<PublicCluster>),
    Stopped,
}
struct Office {
    directory: tempfile::TempDir,
    hub: Arc<Hub>,
    voters: Voters,
    bundle: TrustBundle,
    publisher: Database,
    root: RootKey,
    tenant: Tenant,
}
impl Office {
    async fn in_process() -> Self {
        let directory = tempfile::tempdir().unwrap();
        let cluster = LocalCluster::start(&directory.path().join("voters"), 3)
            .await
            .unwrap();
        let bundle = cluster.bundle().clone();
        Self::open(
            directory,
            Voters::InProcess(Box::new(cluster)),
            bundle,
            Tenant::generate(),
        )
        .await
    }
    async fn as_processes() -> Self {
        let cluster = PublicCluster::start(Limits::default(), 2).await;
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
        let bundle = cluster.bundle.clone();
        Self::open(
            tempfile::tempdir().unwrap(),
            Voters::Processes(Box::new(cluster)),
            bundle,
            tenant.admitted(admission).unwrap(),
        )
        .await
    }
    async fn open(
        directory: tempfile::TempDir,
        voters: Voters,
        bundle: TrustBundle,
        tenant: Tenant,
    ) -> Self {
        let hub = Arc::new(Hub::default());
        let created = configuration(&directory.path().join("publisher"), &hub, "publisher")
            .create()
            .await
            .unwrap();
        Self {
            directory,
            hub,
            voters,
            bundle,
            publisher: created.database,
            root: created.root,
            tenant,
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
        Counters::connect(database.clone(), &self.bundle)
            .await
            .unwrap()
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
    fn local_cluster(&self) -> &LocalCluster {
        match &self.voters {
            Voters::InProcess(cluster) => cluster,
            _ => panic!("this exercise drives running in-process voters"),
        }
    }
    async fn stop_in_process_voters(&mut self) {
        match std::mem::replace(&mut self.voters, Voters::Stopped) {
            Voters::InProcess(cluster) => cluster.shutdown().await,
            _ => panic!("this exercise stops running in-process voters"),
        }
    }
    async fn reopen_in_process_voters(&mut self) {
        let reopened = LocalCluster::open(&self.directory.path().join("voters"))
            .await
            .unwrap();
        assert_eq!(
            reopened.bundle().to_json().unwrap(),
            self.bundle.to_json().unwrap(),
            "a reopened local cluster keeps its genesis and trust bundle"
        );
        self.voters = Voters::InProcess(Box::new(reopened));
    }
    fn processes(&mut self) -> &mut PublicCluster {
        match &mut self.voters {
            Voters::Processes(cluster) => cluster,
            _ => panic!("this exercise stops voter processes"),
        }
    }
    async fn restart_every_voter(&mut self) {
        let cluster = self.processes();
        let admin = cluster.admin();
        for id in 1..=3 {
            cluster.spawn(id, false);
        }
        for id in 1..=3 {
            cluster.wait_online(id, &admin).await;
        }
        cluster.wait_unfenced_leader(&admin).await;
    }
}

fn refused<T: std::fmt::Debug>(result: Result<T, CounterError>) -> Refusal {
    match result {
        Err(CounterError::Coordination(CoordinationError::Denied(refusal))) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn devices_of_a_database_claim_gap_free_numbers_from_in_process_voters_and_a_retry_keeps_its_first()
 {
    let office = Office::in_process().await;
    let (mine, receipt) = office.registered().await;
    let other = office.person("other", Access::Full).await;
    let theirs = office.counters(&other).await;
    let orders = [
        (&mine, Claim::new(b"order-a1", b"order-a1", *ROW_CREATED)),
        (&theirs, Claim::new(b"order-b1", b"order-b1", *ROW_CREATED)),
        (&mine, Claim::new(b"order-a2", b"order-a2", *ROW_CREATED)),
        (&theirs, Claim::new(b"order-b2", b"order-b2", *ROW_CREATED)),
    ];
    let mut numbers = Vec::new();
    for (counters, claim) in &orders {
        numbers.push(counters.claim(&invoices(), claim).await.unwrap());
    }
    assert_eq!(
        numbers,
        [First(1), First(2), First(3), First(4)],
        "gap-free and strictly increasing"
    );
    for ((counters, claim), number) in orders.iter().zip(&numbers) {
        assert_eq!(
            counters.claim(&invoices(), claim).await.unwrap(),
            *number,
            "a retry of the same claim answers its first number"
        );
    }
    assert_eq!(
        theirs
            .claim(
                &invoices(),
                &Claim::new(b"order-a1", b"their-checkout", *ROW_CREATED)
            )
            .await
            .unwrap(),
        Taken(1),
        "another device's attempt on a key in force finds it taken"
    );
    assert_eq!(theirs.read(&invoices()).await.unwrap(), 4);

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
            .claim(
                &invoices(),
                &Claim::new(b"order-b3", b"order-b3", *ROW_CREATED)
            )
            .await
            .unwrap(),
        First(5),
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
    let office = Office::in_process().await;
    let (publisher, _) = office.registered().await;
    let writer = office.person("writer", Access::Full).await;
    let reader = office.person("reader", Access::Read).await;
    let (writing, reading) = (
        office.counters(&writer).await,
        office.counters(&reader).await,
    );
    let first = Claim::new(b"order-1", b"order-1", *ROW_CREATED);
    assert_eq!(writing.claim(&invoices(), &first).await.unwrap(), First(1));
    assert_eq!(
        refused(
            reading
                .claim(
                    &invoices(),
                    &Claim::new(b"order-2", b"order-2", *ROW_CREATED)
                )
                .await
        ),
        Refusal::ReadOnly
    );
    assert_eq!(
        reading.read(&invoices()).await.unwrap(),
        1,
        "a reader still reads the counter"
    );

    let removed = writer.device_credential().await.unwrap().identity;
    office
        .publisher
        .remove(PersonId::new(removed))
        .await
        .unwrap();
    publisher.share_revocations().await.unwrap();
    assert_eq!(
        refused(
            writing
                .claim(
                    &invoices(),
                    &Claim::new(b"order-3", b"order-3", *ROW_CREATED)
                )
                .await
        ),
        Refusal::Revoked
    );
    assert_eq!(
        refused(writing.claim(&invoices(), &first).await),
        Refusal::Revoked,
        "a removed person cannot even replay an earlier claim"
    );
    assert_eq!(
        publisher
            .claim(
                &invoices(),
                &Claim::new(b"order-4", b"order-4", *ROW_CREATED)
            )
            .await
            .unwrap(),
        First(2)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_sent_to_stopped_voters_is_retryable_and_its_retry_takes_one_number() {
    let mut office = Office::as_processes().await;
    let counters = office.registered().await.0.deadline(Duration::from_secs(2));
    let claim = Claim::new(b"order-1", b"order-1", *ROW_CREATED);
    for id in 1..=3 {
        office.processes().stop(id);
    }
    let unreached = counters.claim(&invoices(), &claim).await;
    assert!(
        matches!(
            unreached,
            Err(CounterError::Coordination(
                CoordinationError::Unavailable | CoordinationError::OutcomeUnknown
            ))
        ),
        "a claim over a connection that stops answering is never a success or a refusal: {unreached:?}"
    );
    assert!(unreached.is_err_and(|error| error.retryable()));
    let unread = counters.read(&invoices()).await;
    assert!(
        matches!(
            unread,
            Err(CounterError::Coordination(CoordinationError::Unavailable))
        ),
        "an unanswered read changed nothing: {unread:?}"
    );

    office.restart_every_voter().await;
    let counters = counters.deadline(rocky_coordinator::CALL_DEADLINE);
    assert_eq!(
        counters.read(&invoices()).await.unwrap(),
        0,
        "a claim no voter received took no number"
    );
    assert_eq!(counters.claim(&invoices(), &claim).await.unwrap(), First(1));
    assert_eq!(
        counters.claim(&invoices(), &claim).await.unwrap(),
        First(1),
        "the retry returns the first number"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_doors_scanning_one_ticket_at_once_admit_it_exactly_once() {
    let office = Office::in_process().await;
    office.registered().await;
    let east = office.person("east-door", Access::Full).await;
    let west = office.person("west-door", Access::Full).await;
    let (east, west) = (office.counters(&east).await, office.counters(&west).await);
    let admissions = Name::new("admissions").unwrap();
    let scan = |door: &str| Claim::new(b"ticket-7", format!("{door}-scan-1"), *ROW_CREATED);
    let (east_scan, west_scan) = (scan("east"), scan("west"));
    let (at_east, at_west) = tokio::join!(
        east.claim(&admissions, &east_scan),
        west.claim(&admissions, &west_scan)
    );
    let answers = [at_east.unwrap(), at_west.unwrap()];
    assert_eq!(
        answers
            .iter()
            .filter(|answer| matches!(answer, First(_)))
            .count(),
        1,
        "exactly one door admits the ticket: {answers:?}"
    );
    assert!(answers.iter().all(|answer| answer.number() == 1));
    assert_eq!(
        east.claim(&admissions, &scan("east")).await.unwrap(),
        answers[0],
        "a scan rebuilt from its row repeats its door's answer"
    );
    assert_eq!(
        west.claim(&admissions, &scan("west")).await.unwrap(),
        answers[1]
    );
    if let Voters::InProcess(cluster) = office.voters {
        cluster.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn closed_counters_get_an_error_on_every_clone_and_take_no_number() {
    let office = Office::in_process().await;
    office.registered().await;
    let till = office.counters(&office.publisher).await;
    let other_till = till.clone();
    let order = |id: &str| Claim::new(id, id, *ROW_CREATED);
    assert_eq!(
        till.claim(&invoices(), &order("order-1")).await.unwrap(),
        First(1)
    );
    drop(till.clone());
    assert_eq!(
        till.claim(&invoices(), &order("order-2")).await.unwrap(),
        First(2),
        "a dropped clone leaves the endpoint to the others"
    );

    till.close().await;
    let offline = other_till.claim(&invoices(), &order("order-3")).await;
    assert!(
        matches!(offline, Err(CounterError::Closed)),
        "closing one handle switches off every clone: {offline:?}"
    );
    assert!(
        offline.is_err_and(|error| !error.retryable()),
        "retrying closed counters never brings them back online"
    );
    assert!(matches!(
        other_till.read(&invoices()).await,
        Err(CounterError::Closed)
    ));
    assert!(matches!(
        till.share_revocations().await,
        Err(CounterError::Closed)
    ));
    let online = office.counters(&office.publisher).await;
    assert_eq!(
        online.read(&invoices()).await.unwrap(),
        2,
        "the offline claim took no number"
    );
    if let Voters::InProcess(cluster) = office.voters {
        cluster.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_claim_made_before_the_local_cluster_reopens_is_retried_after_it_and_keeps_its_number() {
    let mut office = Office::in_process().await;
    let (till, _) = office.registered().await;
    let sale = Claim::new(b"sale-1", b"sale-1", *ROW_CREATED);
    assert_eq!(till.claim(&invoices(), &sale).await.unwrap(), First(1));

    office.stop_in_process_voters().await;
    let during_the_outage = till
        .clone()
        .deadline(Duration::from_secs(1))
        .claim(&invoices(), &Claim::new(b"sale-2", b"sale-2", *ROW_CREATED))
        .await;
    assert!(
        during_the_outage.is_err_and(|error| error.retryable()),
        "a stopped authority is a retryable error, never a number"
    );

    office.reopen_in_process_voters().await;
    assert_eq!(
        till.claim(&invoices(), &sale).await.unwrap(),
        First(1),
        "the retry after the reopen answers the number taken before it"
    );
    assert_eq!(
        till.claim(&invoices(), &Claim::new(b"sale-2", b"sale-2", *ROW_CREATED))
            .await
            .unwrap(),
        First(2),
        "the sale retried after the outage takes the next number, leaving no gap"
    );
    office.stop_in_process_voters().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_lost_reply_to_a_committed_claim_is_outcome_unknown_and_the_retry_learns_its_number() {
    let office = Office::in_process().await;
    let (till, _) = office.registered().await;
    let sale = Claim::new(b"sale-1", b"sale-1", *ROW_CREATED);
    office.local_cluster().lose_next_claim_reply();
    let unanswered = till.claim(&invoices(), &sale).await;
    assert!(
        matches!(
            unanswered,
            Err(CounterError::Coordination(
                CoordinationError::OutcomeUnknown
            ))
        ),
        "a committed claim whose reply is lost is OutcomeUnknown: {unanswered:?}"
    );
    assert!(unanswered.is_err_and(|error| error.retryable()));
    assert_eq!(
        till.claim(&invoices(), &sale).await.unwrap(),
        First(1),
        "the same claim sent again learns the number the lost reply held"
    );
    assert_eq!(till.read(&invoices()).await.unwrap(), 1);
    if let Voters::InProcess(cluster) = office.voters {
        cluster.shutdown().await;
    }
}
