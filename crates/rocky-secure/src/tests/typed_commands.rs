//! Typed commands on an encrypted replica: tickets, the handler host
//! contract, shared admission and scoped results.
use super::*;
use crate::credentials::{Authority, device_chain};
use rocky_coordinator::MemoryCoordinator;
use rocky_core::{
    Affinity, Collation, ColumnClass, CommandDefinitions, CommandFailure, CommandFuture,
    CommandHost, Confirmation, CoordinationFuture, Coordinator, DomainId, HandlerInput,
    HandlerOutput, LogicalColumn, LogicalSchema, LogicalTable, Outcome, ROW_IDENTITY_SCHEMA_FORMAT,
    RequestId, RowApplication, SchemaSource, Scope, SqlValue, StrongHead, StrongReceipt,
    TableClass, TableWrite, TypedCommands, TypedRows, UpdatePolicy,
};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};

const COMMANDS: &str = r#"{"format":1,"handler":3,"commands":[
  {"name":"Restock","version":1,"handler":"stock.restock.v1",
   "arguments":[{"name":"item","type":"text"},{"name":"quantity","type":"integer"}],
   "accepted":[{"name":"remaining","type":"integer"}],"rejected":[],
   "invocation":"stock","result":"stock","resources":["stock","holds"]},
  {"name":"Reserve","version":1,"handler":"stock.reserve.v1",
   "arguments":[{"name":"item","type":"text"},{"name":"quantity","type":"integer"}],
   "accepted":[{"name":"remaining","type":"integer"}],
   "rejected":[{"name":"remaining","type":"integer"}],
   "invocation":"holds","result":"holds","resources":["stock","holds"]},
  {"name":"Reserve","version":2,"handler":"stock.reserve.v2",
   "arguments":[{"name":"item","type":"text"},{"name":"quantity","type":"integer"},{"name":"hold","type":"text"}],
   "accepted":[{"name":"remaining","type":"integer"}],
   "rejected":[{"name":"remaining","type":"integer"},{"name":"requested","type":"integer"}],
   "invocation":"holds","result":"holds","resources":["stock","holds"]}]}"#;

pub(super) fn text(value: &str) -> SqlValue {
    SqlValue::Text(value.into())
}
pub(super) fn identity_row(mut row: Vec<SqlValue>) -> Vec<SqlValue> {
    let key = format!("{:?}", row[0]);
    let sequence = key.bytes().fold(1u32, |sum, byte| {
        sum.wrapping_mul(31).wrapping_add(u32::from(byte))
    });
    row.push(SqlValue::Integer(
        rocky_core::RowIdentity::new(1, sequence.max(1))
            .unwrap()
            .integer(),
    ));
    row.push(SqlValue::Integer(0));
    row
}
pub(super) fn integer(value: &SqlValue) -> i64 {
    match value {
        SqlValue::Integer(value) => *value,
        other => panic!("not an integer: {other:?}"),
    }
}

/// What a test makes the next handler call do instead of deciding.
enum Fault {
    Fail(CommandFailure),
    Decide(HandlerOutput),
    /// Runs while the handler holds its input, then decides normally.
    Meanwhile(Box<dyn FnOnce() + Send>),
}
/// The reservation handler: stock never goes below zero.
#[derive(Default)]
pub(super) struct Reservations {
    calls: AtomicUsize,
    running: AtomicUsize,
    overlapped: AtomicUsize,
    fault: Mutex<Option<Fault>>,
    inputs: Mutex<Vec<HandlerInput>>,
}
impl Reservations {
    fn inject(&self, fault: Fault) {
        *self.fault.lock().unwrap() = Some(fault);
    }
    fn decide(input: &HandlerInput) -> HandlerOutput {
        let item = input.arguments[0].clone();
        let quantity = integer(&input.arguments[1]);
        let remaining = input
            .reads
            .get("stock", std::slice::from_ref(&item))
            .unwrap()
            .as_ref()
            .map_or(0, |row| integer(&row[1]));
        let stock = |remaining| TableWrite::Put {
            table: "stock".into(),
            row: identity_row(vec![item.clone(), SqlValue::Integer(remaining)]),
        };
        if input.method == "Restock" {
            return HandlerOutput {
                outcome: Outcome::Accepted(vec![SqlValue::Integer(remaining + quantity)]),
                writes: vec![stock(remaining + quantity)],
                jobs: Vec::new(),
            };
        }
        if quantity <= 0 || quantity > remaining {
            let mut rejected = vec![SqlValue::Integer(remaining)];
            if input.version == 2 {
                rejected.push(SqlValue::Integer(quantity));
            }
            return HandlerOutput {
                outcome: Outcome::Rejected(rejected),
                writes: Vec::new(),
                jobs: Vec::new(),
            };
        }
        let hold = match input.arguments.get(2) {
            Some(hold) => hold.clone(),
            None => text(&format!("request-{}", input.request.sequence.get())),
        };
        HandlerOutput {
            outcome: Outcome::Accepted(vec![SqlValue::Integer(remaining - quantity)]),
            writes: vec![
                stock(remaining - quantity),
                TableWrite::Put {
                    table: "holds".into(),
                    row: identity_row(vec![hold, item.clone(), SqlValue::Integer(quantity)]),
                },
            ],
            jobs: vec![("holds".into(), b"notify".to_vec())],
        }
    }
}
impl CommandHost for Reservations {
    fn handle(&self, input: HandlerInput) -> CommandFuture<'_, HandlerOutput> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.running.fetch_add(1, Ordering::SeqCst) > 0 {
                self.overlapped.fetch_add(1, Ordering::SeqCst);
            }
            // Lets another execution run if nothing serializes them.
            tokio::task::yield_now().await;
            self.inputs.lock().unwrap().push(input.clone());
            let fault = self.fault.lock().unwrap().take();
            let decision = match fault {
                Some(Fault::Fail(failure)) => Err(failure),
                Some(Fault::Decide(output)) => Ok(output),
                Some(Fault::Meanwhile(action)) => {
                    action();
                    Ok(Self::decide(&input))
                }
                None => Ok(Self::decide(&input)),
            };
            self.running.fetch_sub(1, Ordering::SeqCst);
            decision
        })
    }
}

/// Records what reaches the coordinator.
struct Recording {
    inner: Arc<MemoryCoordinator>,
    proposals: Mutex<Vec<Vec<u8>>>,
}
impl Coordinator for Recording {
    fn head(&self, scope: Scope) -> CoordinationFuture<'_, StrongHead> {
        Coordinator::head(&*self.inner, scope)
    }
    fn confirm(&self, proposal: Confirmation) -> CoordinationFuture<'_, StrongReceipt> {
        self.proposals.lock().unwrap().push(proposal.encode());
        Coordinator::confirm(&*self.inner, proposal)
    }
    fn lookup(&self, scope: Scope, id: RequestId) -> CoordinationFuture<'_, Option<StrongReceipt>> {
        Coordinator::lookup(&*self.inner, scope, id)
    }
    fn verify<'a>(&'a self, receipt: &'a StrongReceipt) -> CoordinationFuture<'a, ()> {
        Coordinator::verify(&*self.inner, receipt)
    }
    fn retire(
        &self,
        scope: Scope,
        node: rocky_core::NodeId,
        through: u64,
    ) -> CoordinationFuture<'_, ()> {
        Coordinator::retire(&*self.inner, scope, node, through)
    }
}

pub(super) struct Device {
    pub(super) node: PrivateKey,
    user: PrivateKey,
}
impl Device {
    pub(super) fn new() -> Self {
        Self {
            node: PrivateKey::new(),
            user: PrivateKey::new(),
        }
    }
    fn id(&self) -> rocky_core::NodeId {
        secure_node_id(self.node.public_key())
    }
    fn principal(&self) -> rocky_core::PrincipalId {
        secure_principal_id(self.user.public_key())
    }
}
pub(super) struct World {
    pub(super) publisher: PrivateKey,
    pub(super) schema: LogicalSchema,
    pub(super) domain: Scope,
    pub(super) stock: ResourceScope,
    pub(super) holds: ResourceScope,
    dir: TempDir,
    pub(super) coordinator: Arc<MemoryCoordinator>,
    pub(super) executor: Device,
    /// Eligible to execute after a takeover.
    successor: Device,
    pub(super) handler: Arc<Reservations>,
}
fn column(name: &str, affinity: Affinity) -> LogicalColumn {
    LogicalColumn {
        logical: name.into(),
        sql_name: name.into(),
        affinity,
        not_null: true,
        default: None,
        collation: Collation::Binary,
        class: ColumnClass::Mergeable,
    }
}
fn table(name: &str, sql: &str, mut columns: Vec<LogicalColumn>) -> LogicalTable {
    columns.push(column("id", Affinity::Integer));
    let mut kind = column("row_kind", Affinity::Integer);
    kind.default = Some("0".into());
    columns.push(kind);
    let sql = format!(
        "{},id INTEGER NOT NULL,row_kind INTEGER NOT NULL DEFAULT 0)",
        sql.trim_end_matches(')')
    );
    LogicalTable {
        logical: name.into(),
        sql_name: name.into(),
        class: TableClass::Mergeable,
        create_sql: sql,
        auto_generate: false,
        primary_key: vec![columns[0].sql_name.clone()],
        partition_by: Vec::new(),
        columns,
        indexes: vec![rocky_core::LogicalIndex {
            name: format!("{name}_identity"),
            unique: true,
            columns: vec![rocky_core::IndexColumn {
                name: "id".into(),
                descending: false,
            }],
            create_sql: format!("CREATE UNIQUE INDEX {name}_identity ON {name}(id)"),
            on_conflict: None,
        }],
        foreign_keys: Vec::new(),
        reuses_keys: false,
        existence_log: false,
    }
}
const READ: &[Right] = &[Right::Read, Right::Custody];
pub(super) const WRITE: &[Right] = &[
    Right::Read,
    Right::Custody,
    Right::Insert,
    Right::Update,
    Right::Delete,
    Right::Invoke,
];
const EXECUTE: &[Right] = &[
    Right::Read,
    Right::Custody,
    Right::Insert,
    Right::Update,
    Right::Delete,
    Right::Invoke,
    Right::Execute,
];
impl World {
    fn new() -> Self {
        Self::with_stock(TableClass::Mergeable)
    }
    /// The world with `stock` of `class`; `holds` stays mergeable.
    pub(super) fn with_stock(class: TableClass) -> Self {
        let publisher = PrivateKey::new();
        let schema = LogicalSchema {
            format: ROW_IDENTITY_SCHEMA_FORMAT,
            publisher: *publisher.public_key().as_bytes(),
            application: "reservations".into(),
            source: SchemaSource {
                kind: "test".into(),
                version: 1,
                identity: "reservations".into(),
            },
            update_policy: UpdatePolicy::Changed,
            tables: vec![
                table(
                    "holds",
                    "CREATE TABLE holds(key TEXT NOT NULL PRIMARY KEY,item TEXT NOT NULL,quantity INTEGER NOT NULL)",
                    vec![
                        column("key", Affinity::Text),
                        column("item", Affinity::Text),
                        column("quantity", Affinity::Integer),
                    ],
                ),
                table(
                    "stock",
                    "CREATE TABLE stock(item TEXT NOT NULL PRIMARY KEY,remaining INTEGER NOT NULL)",
                    vec![
                        column("item", Affinity::Text),
                        column("remaining", Affinity::Integer),
                    ],
                ),
            ],
            views: Vec::new(),
        };
        let mut schema = schema;
        schema.tables[1].class = class;
        let domain = Scope {
            mesh: MeshId([1; 16]),
            app: schema.app_id(),
            domain: DomainId([1; 16]),
        };
        let scope = |name: &str| ResourceScope {
            domain,
            encryption: rocky_core::commitment("reservations.readers", name.as_bytes()),
            table: schema.table_id(schema.table(name).unwrap()),
        };
        let world = Self {
            stock: scope("stock"),
            holds: scope("holds"),
            publisher,
            schema,
            domain,
            dir: TempDir::new().unwrap(),
            coordinator: Arc::new(MemoryCoordinator::default()),
            executor: Device::new(),
            successor: Device::new(),
            handler: Arc::default(),
        };
        world
            .coordinator
            .register(
                domain,
                world.executor.principal(),
                [1; 16],
                vec![world.executor.id(), world.successor.id()],
                1,
                3,
            )
            .unwrap();
        world
            .coordinator
            .acquire(domain, world.executor.principal(), world.executor.id())
            .unwrap();
        world
    }
    pub(super) fn app(&self, host: Option<Arc<dyn CommandHost>>) -> TypedCommands {
        TypedCommands::new(
            TypedRows::new(self.schema.clone(), vec![self.domain.domain]).unwrap(),
            CommandDefinitions::parse(COMMANDS.as_bytes())
                .unwrap()
                .build()
                .unwrap(),
            [
                ("stock".to_string(), self.stock),
                ("holds".to_string(), self.holds),
            ]
            .into(),
            host,
        )
        .unwrap()
    }
    pub(super) fn manifest(&self) -> rocky_core::ApplicationManifest {
        self.app(None).manifest()
    }
    /// Whole-scope grants over every field and method of `scopes`.
    pub(super) fn grants(&self, scopes: &[(ResourceScope, &[Right])]) -> Vec<ResourceGrant> {
        let manifest = self.manifest();
        let fields: Vec<Commitment> = manifest
            .fields
            .iter()
            .chain(&manifest.methods)
            .map(|item| item.id)
            .collect();
        scopes
            .iter()
            .map(|(scope, rights)| grant(*scope, rights, &fields))
            .collect()
    }
    pub(super) fn config(&self, device: &Device, grants: Vec<ResourceGrant>) -> SecureConfig {
        let manifest = self.manifest();
        let authority = Authority {
            publisher: &self.publisher,
            manifest: manifest.hash(),
        };
        let keys = grants
            .iter()
            .filter(|grant| grant.rights.contains(&Right::Read))
            .map(|grant| KeyMaterial {
                scope: grant.scope,
                generation: 1,
                key: if grant.scope == self.stock {
                    [11; 32]
                } else {
                    [22; 32]
                },
            })
            .collect();
        let root = authority
            .issue(
                device.user.public_key(),
                grants,
                1,
                (now().unwrap(), 3600, 60),
            )
            .unwrap();
        SecureConfig {
            publisher: self.publisher.public_key(),
            manifest: SignedManifest::sign(&manifest, &self.publisher).unwrap(),
            policy: SignedPolicy::sign(
                PolicyStatement {
                    manifest: manifest.hash(),
                    epoch: 1,
                    revoked: BTreeSet::new(),
                },
                &self.publisher,
            )
            .unwrap(),
            node: device.node.clone(),
            credentials: device_chain(root, &device.user, device.node.public_key()).unwrap(),
            keys,
        }
    }
    pub(super) async fn open(
        &self,
        name: &str,
        device: &Device,
        scopes: &[(ResourceScope, &[Right])],
        host: Option<Arc<dyn CommandHost>>,
    ) -> EncryptedReplica<TypedCommands> {
        EncryptedReplica::open(
            self.dir.path().join(name),
            self.app(host),
            self.config(device, self.grants(scopes)),
        )
        .await
        .unwrap()
    }
    pub(super) async fn executor(&self) -> EncryptedReplica<TypedCommands> {
        let host: Arc<dyn CommandHost> = self.handler.clone();
        self.open(
            "executor.db",
            &self.executor,
            &[(self.stock, EXECUTE), (self.holds, EXECUTE)],
            Some(host),
        )
        .await
    }
    /// A writer that invokes and reads both scopes, and cannot execute.
    pub(super) async fn caller(
        &self,
        name: &str,
        device: &Device,
    ) -> EncryptedReplica<TypedCommands> {
        let invokers = vec![
            (self.executor.id(), self.executor.principal()),
            (device.id(), device.principal()),
        ];
        self.coordinator
            .set_invokers(self.domain, self.executor.principal(), invokers)
            .unwrap();
        self.open(
            name,
            device,
            &[(self.stock, WRITE), (self.holds, WRITE)],
            None,
        )
        .await
    }
    pub(super) fn arguments(&self, method: &str, version: u32, values: &[SqlValue]) -> Vec<u8> {
        self.app(None)
            .commands()
            .get(method, version)
            .unwrap()
            .encode_arguments(values)
            .unwrap()
    }
    pub(super) fn reserve(&self, quantity: i64, hold: &str) -> Vec<u8> {
        self.arguments(
            "Reserve",
            2,
            &[text("bolt"), SqlValue::Integer(quantity), text(hold)],
        )
    }
    fn outcome(&self, ticket: &Ticket) -> Outcome {
        let TicketResult::Available(bytes) = &ticket.result else {
            panic!("no result: {ticket:?}");
        };
        self.app(None)
            .commands()
            .get(&ticket.method, ticket.version)
            .unwrap()
            .decode_outcome(bytes)
            .unwrap()
    }
    pub(super) async fn restock(&self, executor: &EncryptedReplica<TypedCommands>, quantity: i64) {
        let handle = rocky_core::commitment("restock", &self.sequence().to_le_bytes());
        let arguments = self.arguments("Restock", 1, &[text("bolt"), SqlValue::Integer(quantity)]);
        executor
            .submit_command(handle, self.domain, "Restock", 1, &arguments)
            .await
            .unwrap();
        let ticket = executor
            .run_ticket(handle, &*self.coordinator)
            .await
            .unwrap();
        assert_eq!(ticket.state, TicketState::Applied);
    }
    pub(super) fn sequence(&self) -> u64 {
        self.coordinator.head(self.domain).unwrap().sequence
    }
    async fn remaining(&self, replica: &EncryptedReplica<TypedCommands>) -> i64 {
        let snapshot = replica.command_snapshot(self.stock).await.unwrap();
        assert!(snapshot.complete);
        let rows = self.app(None).typed_rows("stock", &snapshot.rows).unwrap();
        integer(&rows[0][1])
    }
    /// Gives `to` the confirmed results it lacks, as the device `peer`.
    pub(super) async fn catch_up(
        &self,
        from: &EncryptedReplica<TypedCommands>,
        to: &EncryptedReplica<TypedCommands>,
        peer: &Device,
        scopes: &[ResourceScope],
    ) {
        let credentials = to.inner.lock().await.config.credentials.clone();
        for sequence in 1..=self.sequence() {
            let result = from
                .encrypted_result_for(
                    peer.node.public_key(),
                    &credentials,
                    self.domain,
                    sequence,
                    scopes,
                )
                .await
                .unwrap()
                .unwrap();
            to.receive_encrypted_result(result, &*self.coordinator)
                .await
                .unwrap();
        }
    }
}

#[tokio::test]
async fn competing_reservations_confirm_one_acceptance_and_one_typed_rejection() {
    let world = World::new();
    let executor = world.executor().await;
    let other = Device::new();
    let caller = world.caller("caller.db", &other).await;
    world.restock(&executor, 1).await;

    // Both submit before either executes; submission is local and durable.
    let mine = executor
        .submit_command(
            [1; 32],
            world.domain,
            "Reserve",
            2,
            &world.reserve(1, "mine"),
        )
        .await
        .unwrap();
    let theirs = caller
        .submit_command(
            [2; 32],
            world.domain,
            "Reserve",
            2,
            &world.reserve(1, "theirs"),
        )
        .await
        .unwrap();
    assert_eq!(mine.state, TicketState::Queued);
    assert_eq!(theirs.state, TicketState::Queued);
    assert_eq!(theirs.result, TicketResult::Pending);
    assert_eq!(world.sequence(), 1);
    // The same handle with the same input is the same ticket; another input conflicts.
    assert_eq!(
        caller
            .submit_command(
                [2; 32],
                world.domain,
                "Reserve",
                2,
                &world.reserve(1, "theirs")
            )
            .await
            .unwrap(),
        theirs
    );
    assert!(matches!(
        caller
            .submit_command(
                [2; 32],
                world.domain,
                "Reserve",
                2,
                &world.reserve(2, "theirs")
            )
            .await,
        Err(Error::Conflict)
    ));

    let mine = executor
        .run_ticket([1; 32], &*world.coordinator)
        .await
        .unwrap();
    assert_eq!(mine.state, TicketState::Applied);
    assert_eq!(
        world.outcome(&mine),
        Outcome::Accepted(vec![SqlValue::Integer(0)])
    );

    // The other caller's command is delivered as bytes and settled from the answer.
    let envelope = caller.export_ticket([2; 32]).await.unwrap();
    assert_eq!(
        caller.ticket([2; 32]).await.unwrap().unwrap().state,
        TicketState::OutcomeUnknown
    );
    let answer = executor
        .deliver_command(&envelope.to_bytes().unwrap(), &*world.coordinator)
        .await;
    let answer = Delivery::from_bytes(&answer.to_bytes().unwrap()).unwrap();
    let settled = caller
        .settle_delivery([2; 32], answer, &*world.coordinator)
        .await
        .unwrap();
    // Confirmed with a receipt, but its predecessors are not here yet.
    assert_eq!(settled.state, TicketState::Confirmed);
    assert_eq!(settled.sequence, Some(3));
    assert_eq!(settled.request, theirs.request);
    world
        .catch_up(&executor, &caller, &other, &[world.stock, world.holds])
        .await;
    let settled = caller.ticket([2; 32]).await.unwrap().unwrap();
    assert_eq!(settled.state, TicketState::Applied);
    // A business rejection is a confirmed typed result, not an error.
    assert_eq!(
        world.outcome(&settled),
        Outcome::Rejected(vec![SqlValue::Integer(0), SqlValue::Integer(1)])
    );

    for replica in [&executor, &caller] {
        assert_eq!(world.remaining(replica).await, 0);
        let holds = replica.command_snapshot(world.holds).await.unwrap();
        let holds = world.app(None).typed_rows("holds", &holds.rows).unwrap();
        assert_eq!(
            holds,
            vec![identity_row(vec![
                text("mine"),
                text("bolt"),
                SqlValue::Integer(1)
            ])]
        );
    }
    assert_eq!(world.sequence(), 3);
    assert!(caller.open_tickets(8).await.unwrap().is_empty());
    // A protocol failure is an error and leaves no ticket.
    assert!(matches!(
        caller
            .submit_command([3; 32], world.domain, "Reserve", 7, &world.reserve(1, "x"))
            .await,
        Err(Error::Incompatible(_))
    ));
    assert_eq!(caller.ticket([3; 32]).await.unwrap(), None);
    // The handler saw typed input bound to the predecessor it decided on.
    let inputs = world.handler.inputs.lock().unwrap();
    assert_eq!(inputs.len(), 3);
    assert_eq!(inputs[2].handler, "stock.reserve.v2");
    assert_eq!(inputs[2].predecessor.0, 2);
    assert_eq!(inputs[2].caller, other.principal());
    assert_eq!(
        inputs[2].reads.scan("stock", None, None, 10).unwrap(),
        vec![identity_row(vec![text("bolt"), SqlValue::Integer(0)])]
    );
}

#[tokio::test]
async fn local_and_delivered_commands_share_admission_and_forgeries_fail() {
    let world = World::new();
    let executor = world.executor().await;
    let other = Device::new();
    let caller = world.caller("caller.db", &other).await;
    world.restock(&executor, 5).await;
    let refused = |answer: Delivery, expected: &str| match answer {
        Delivery::NotSubmitted { retryable, message } => {
            assert!(!retryable, "{message}");
            assert!(message.contains(expected), "{message}");
        }
        other => panic!("not refused: {other:?}"),
    };

    // A version outside the horizon, signed by an authorized caller.
    let future = caller
        .forge_command([1; 32], world.holds, "Reserve", 9, &world.reserve(1, "a"))
        .await
        .unwrap();
    assert!(matches!(
        executor.admit_command(&future.to_bytes().unwrap()).await,
        Err(Error::Incompatible(_))
    ));
    refused(
        executor
            .deliver_command(&future.to_bytes().unwrap(), &*world.coordinator)
            .await,
        "support horizon",
    );
    // The local path refuses it at the same check.
    let own = executor
        .forge_command([2; 32], world.holds, "Reserve", 9, &world.reserve(1, "a"))
        .await
        .unwrap();
    assert!(matches!(
        executor.execute_encrypted(own, &*world.coordinator).await,
        Err(Error::Incompatible(_))
    ));

    // Sealed arguments of another version, and bytes that are no arguments.
    for payload in [
        world.arguments("Reserve", 1, &[text("bolt"), SqlValue::Integer(1)]),
        b"1 bolt please".to_vec(),
    ] {
        let malformed = caller
            .forge_command([3; 32], world.holds, "Reserve", 2, &payload)
            .await
            .unwrap();
        refused(
            executor
                .deliver_command(&malformed.to_bytes().unwrap(), &*world.coordinator)
                .await,
            "Reserve",
        );
        // The caller's own runtime refuses to seal them at all.
        assert!(
            caller
                .submit_command([3; 32], world.domain, "Reserve", 2, &payload)
                .await
                .is_err()
        );
    }
    // An unregistered method, and arguments sealed for the wrong scope.
    let unknown = caller
        .forge_command([4; 32], world.holds, "Drain", 1, b"all")
        .await
        .unwrap();
    refused(
        executor
            .deliver_command(&unknown.to_bytes().unwrap(), &*world.coordinator)
            .await,
        "unregistered",
    );
    let exposed = caller
        .forge_command([5; 32], world.stock, "Reserve", 2, &world.reserve(1, "a"))
        .await
        .unwrap();
    refused(
        executor
            .deliver_command(&exposed.to_bytes().unwrap(), &*world.coordinator)
            .await,
        "invocation scope",
    );
    // A changed envelope no longer carries its caller's signature.
    let genuine = caller
        .submit_command([6; 32], world.domain, "Reserve", 2, &world.reserve(1, "a"))
        .await
        .unwrap();
    let mut tampered = caller
        .export_ticket([6; 32])
        .await
        .unwrap()
        .to_bytes()
        .unwrap();
    let last = tampered.len() - 1;
    tampered[last] ^= 1;
    assert!(matches!(
        executor
            .deliver_command(&tampered, &*world.coordinator)
            .await,
        Delivery::NotSubmitted {
            retryable: false,
            ..
        }
    ));

    // Grants: a reader cannot invoke, with or without its own runtime's consent.
    let reader_device = Device::new();
    let reader = world
        .open(
            "reader.db",
            &reader_device,
            &[(world.stock, READ), (world.holds, READ)],
            None,
        )
        .await;
    assert!(
        reader
            .submit_command([7; 32], world.domain, "Reserve", 2, &world.reserve(1, "r"))
            .await
            .is_err()
    );
    let ungranted = reader
        .forge_command([7; 32], world.holds, "Reserve", 2, &world.reserve(1, "r"))
        .await
        .unwrap();
    refused(
        executor
            .deliver_command(&ungranted.to_bytes().unwrap(), &*world.coordinator)
            .await,
        "invocation denied",
    );

    // Execution: holding a handler and the bytes is not eligibility.
    let host: Arc<dyn CommandHost> = Arc::new(Reservations::default());
    let pretender_device = Device::new();
    let pretender = world
        .open(
            "pretender.db",
            &pretender_device,
            &[(world.stock, EXECUTE), (world.holds, EXECUTE)],
            Some(host.clone()),
        )
        .await;
    world
        .catch_up(
            &executor,
            &pretender,
            &pretender_device,
            &[world.stock, world.holds],
        )
        .await;
    let envelope = caller
        .export_ticket([6; 32])
        .await
        .unwrap()
        .to_bytes()
        .unwrap();
    // Granted `Execute`, but the coordinator did not assign it.
    refused(
        pretender
            .deliver_command(&envelope, &*world.coordinator)
            .await,
        "fenced",
    );
    // Assigned by the coordinator, but without `Execute` in its credentials.
    let ungranted_device = Device::new();
    let assigned = MemoryCoordinator::default();
    assigned
        .register(
            world.domain,
            world.executor.principal(),
            [1; 16],
            vec![ungranted_device.id()],
            1,
            3,
        )
        .unwrap();
    assigned
        .acquire(
            world.domain,
            world.executor.principal(),
            ungranted_device.id(),
        )
        .unwrap();
    let ungranted_executor = world
        .open(
            "ungranted.db",
            &ungranted_device,
            &[(world.stock, WRITE), (world.holds, WRITE)],
            Some(host),
        )
        .await;
    refused(
        ungranted_executor
            .deliver_command(&envelope, &assigned)
            .await,
        "execution scope denied",
    );
    assert_eq!(assigned.head(world.domain).unwrap().sequence, 0);

    // Nothing above reached the coordinator or changed state.
    assert_eq!(world.sequence(), 1);
    assert_eq!(world.remaining(&executor).await, 5);
    // The genuine ticket still executes, through the same admission.
    let answer = executor
        .deliver_command(&envelope, &*world.coordinator)
        .await;
    let settled = caller
        .settle_delivery([6; 32], answer, &*world.coordinator)
        .await
        .unwrap();
    assert_eq!(settled.request, genuine.request);
    assert_eq!(settled.sequence, Some(2));
}

#[tokio::test]
async fn lost_reply_reopen_cancellation_and_takeover_recover_the_same_ticket() {
    let world = World::new();
    let executor = world.executor().await;
    world.restock(&executor, 2).await;
    let submitted = executor
        .submit_command(
            [1; 32],
            world.domain,
            "Reserve",
            2,
            &world.reserve(1, "first"),
        )
        .await
        .unwrap();

    // The coordinator accepts and its reply is lost.
    world.coordinator.drop_next_reply().unwrap();
    assert!(
        executor
            .run_ticket([1; 32], &*world.coordinator)
            .await
            .is_err()
    );
    let unknown = executor.ticket([1; 32]).await.unwrap().unwrap();
    assert_eq!(unknown.state, TicketState::OutcomeUnknown);
    assert_eq!(unknown.result, TicketResult::Pending);
    assert_eq!(world.sequence(), 2);
    // Cancellation cannot withdraw what may already be confirmed.
    assert_eq!(
        executor.cancel_ticket([1; 32]).await.unwrap().state,
        TicketState::OutcomeUnknown
    );
    drop(executor);
    let executor = world.executor().await;
    let reopened = executor.ticket([1; 32]).await.unwrap().unwrap();
    assert_eq!(reopened, unknown);
    assert_eq!(reopened.request, submitted.request);
    assert_eq!(executor.open_tickets(8).await.unwrap(), vec![reopened]);

    let calls = world.handler.calls.load(Ordering::SeqCst);
    let recovered = executor
        .run_ticket([1; 32], &*world.coordinator)
        .await
        .unwrap();
    assert_eq!(recovered.state, TicketState::Applied);
    assert_eq!(recovered.request, submitted.request);
    assert_eq!(recovered.sequence, Some(2));
    assert_eq!(
        world.outcome(&recovered),
        Outcome::Accepted(vec![SqlValue::Integer(1)])
    );
    // The retained preparation was confirmed: no second decision or transition.
    assert_eq!(world.handler.calls.load(Ordering::SeqCst), calls);
    assert_eq!(world.sequence(), 2);
    assert_eq!(
        executor
            .run_ticket([1; 32], &*world.coordinator)
            .await
            .unwrap(),
        recovered
    );
    assert_eq!(world.sequence(), 2);

    // A ticket that never left the device can be withdrawn, for good.
    executor
        .submit_command(
            [2; 32],
            world.domain,
            "Reserve",
            2,
            &world.reserve(1, "second"),
        )
        .await
        .unwrap();
    assert_eq!(
        executor.cancel_ticket([2; 32]).await.unwrap().state,
        TicketState::Cancelled
    );
    assert!(
        executor
            .run_ticket([2; 32], &*world.coordinator)
            .await
            .is_err()
    );
    assert!(executor.export_ticket([2; 32]).await.is_err());
    assert_eq!(
        executor
            .submit_command(
                [2; 32],
                world.domain,
                "Reserve",
                2,
                &world.reserve(1, "second")
            )
            .await
            .unwrap()
            .state,
        TicketState::Cancelled
    );
    assert!(executor.open_tickets(8).await.unwrap().is_empty());
    assert_eq!(world.sequence(), 2);

    // Takeover: the fenced executor's prepared attempt is never confirmed.
    let third = executor
        .submit_command(
            [3; 32],
            world.domain,
            "Reserve",
            2,
            &world.reserve(1, "third"),
        )
        .await
        .unwrap();
    let envelope = executor.export_ticket([3; 32]).await.unwrap();
    let stale = executor
        .prepare_encrypted(envelope.clone(), &*world.coordinator)
        .await
        .unwrap();
    let successor_handler = Arc::new(Reservations::default());
    let host: Arc<dyn CommandHost> = successor_handler.clone();
    let successor = world
        .open(
            "successor.db",
            &world.successor,
            &[(world.stock, EXECUTE), (world.holds, EXECUTE)],
            Some(host),
        )
        .await;
    world
        .catch_up(
            &executor,
            &successor,
            &world.successor,
            &[world.stock, world.holds],
        )
        .await;
    let epoch = world.coordinator.head(world.domain).unwrap().epoch;
    world
        .coordinator
        .takeover(
            world.domain,
            world.executor.principal(),
            world.successor.id(),
            epoch,
        )
        .unwrap();
    assert!(
        executor
            .run_ticket([3; 32], &*world.coordinator)
            .await
            .is_err()
    );
    assert!(
        executor
            .confirm_encrypted(
                stale.clone(),
                &*world.coordinator,
                &DurabilityPolicy::AsyncReplication,
                &[]
            )
            .await
            .is_err()
    );
    assert_eq!(world.sequence(), 2);
    let answer = successor
        .deliver_command(&envelope.to_bytes().unwrap(), &*world.coordinator)
        .await;
    let recovered = executor
        .settle_delivery([3; 32], answer, &*world.coordinator)
        .await
        .unwrap();
    assert_eq!(recovered.state, TicketState::Applied);
    assert_eq!(recovered.request, third.request);
    assert_eq!(recovered.sequence, Some(3));
    assert_eq!(successor_handler.calls.load(Ordering::SeqCst), 1);
    // The fenced attempt stays unconfirmed, and the ticket stays settled.
    assert!(
        executor
            .confirm_encrypted(
                stale,
                &*world.coordinator,
                &DurabilityPolicy::AsyncReplication,
                &[]
            )
            .await
            .is_err()
    );
    assert_eq!(
        executor
            .run_ticket([3; 32], &*world.coordinator)
            .await
            .unwrap(),
        recovered
    );
    assert_eq!(world.sequence(), 3);
    for replica in [&executor, &successor] {
        assert_eq!(world.remaining(replica).await, 0);
    }
}

#[tokio::test]
async fn handler_failures_and_a_moved_head_prepare_nothing_and_stay_retryable() {
    let world = World::new();
    let executor = world.executor().await;
    world.restock(&executor, 3).await;
    executor
        .submit_command([1; 32], world.domain, "Reserve", 2, &world.reserve(1, "a"))
        .await
        .unwrap();
    let preparations = async |replica: &EncryptedReplica<TypedCommands>| {
        let inner = replica.inner.lock().await;
        crate::storage::rows(&inner.conn, "SELECT request FROM secure_preparations")
            .await
            .unwrap()
            .len()
    };
    let retained = preparations(&executor).await;

    // A missed deadline or a failing callback, as its host reports it.
    world.handler.inject(Fault::Fail(CommandFailure::Retryable(
        "deadline of 10 s passed".into(),
    )));
    let failure = executor
        .run_ticket([1; 32], &*world.coordinator)
        .await
        .unwrap_err();
    assert!(matches!(failure, Error::Handler(_)), "{failure}");
    assert!(failure.is_retryable());

    // Output that breaks the contract is refused before anything is prepared.
    let accepted = || Outcome::Accepted(vec![SqlValue::Integer(2)]);
    let put = |table: &str, row: Vec<SqlValue>| TableWrite::Put {
        table: table.into(),
        row: identity_row(row),
    };
    let stock = |remaining| put("stock", vec![text("bolt"), SqlValue::Integer(remaining)]);
    for (output, expected) in [
        (
            HandlerOutput {
                outcome: Outcome::Accepted(vec![text("two")]),
                writes: vec![stock(2)],
                jobs: Vec::new(),
            },
            "another type",
        ),
        (
            HandlerOutput {
                outcome: Outcome::Rejected(vec![SqlValue::Integer(3), SqlValue::Integer(1)]),
                writes: vec![stock(2)],
                jobs: Vec::new(),
            },
            "rejection changes no rows",
        ),
        (
            HandlerOutput {
                outcome: accepted(),
                writes: vec![put("audit", vec![text("x")])],
                jobs: Vec::new(),
            },
            "outside the resources",
        ),
        (
            HandlerOutput {
                outcome: accepted(),
                writes: vec![stock(2), stock(1)],
                jobs: Vec::new(),
            },
            "written twice",
        ),
        (
            HandlerOutput {
                outcome: accepted(),
                writes: vec![put("stock", vec![text("bolt"), text("two")])],
                jobs: Vec::new(),
            },
            "declared type",
        ),
        (
            HandlerOutput {
                outcome: accepted(),
                writes: vec![TableWrite::Delete {
                    table: "holds".into(),
                    key: vec![text("absent")],
                }],
                jobs: Vec::new(),
            },
            "absent",
        ),
    ] {
        world.handler.inject(Fault::Decide(output));
        let failure = executor
            .run_ticket([1; 32], &*world.coordinator)
            .await
            .unwrap_err();
        assert!(!failure.is_retryable(), "{failure}");
        assert!(failure.to_string().contains(expected), "{failure}");
    }

    // The fence moves while the handler decides: its decision is dropped.
    let coordinator = world.coordinator.clone();
    let (domain, control, node) = (
        world.domain,
        world.executor.principal(),
        world.executor.id(),
    );
    world.handler.inject(Fault::Meanwhile(Box::new(move || {
        let epoch = coordinator.head(domain).unwrap().epoch;
        coordinator.takeover(domain, control, node, epoch).unwrap();
    })));
    let failure = executor
        .run_ticket([1; 32], &*world.coordinator)
        .await
        .unwrap_err();
    assert!(
        matches!(failure, Error::StrongPending(PendingReason::StaleFence)),
        "{failure}"
    );
    assert!(failure.is_retryable());

    // Every failure left the ticket queued, with nothing prepared or confirmed.
    assert_eq!(
        executor.ticket([1; 32]).await.unwrap().unwrap().state,
        TicketState::Queued
    );
    assert_eq!(preparations(&executor).await, retained);
    assert_eq!(world.sequence(), 1);
    assert_eq!(world.remaining(&executor).await, 3);
    // The same ticket then runs once, at the new epoch.
    let ticket = executor
        .run_ticket([1; 32], &*world.coordinator)
        .await
        .unwrap();
    assert_eq!(ticket.state, TicketState::Applied);
    assert_eq!(world.remaining(&executor).await, 2);

    // Executions on one replica run one at a time, each on the other's result.
    for (handle, hold) in [([2; 32], "b"), ([3; 32], "c")] {
        executor
            .submit_command(handle, world.domain, "Reserve", 2, &world.reserve(1, hold))
            .await
            .unwrap();
    }
    let (second, third) = tokio::join!(
        executor.run_ticket([2; 32], &*world.coordinator),
        executor.run_ticket([3; 32], &*world.coordinator)
    );
    let mut sequences = vec![second.unwrap().sequence, third.unwrap().sequence];
    sequences.sort();
    assert_eq!(sequences, [Some(3), Some(4)]);
    assert_eq!(world.handler.overlapped.load(Ordering::SeqCst), 0);
    assert_eq!(world.remaining(&executor).await, 0);

    // Unresolved tickets are bounded; a resolved or cancelled one frees a place.
    for index in 0..MAX_OPEN_TICKETS {
        executor
            .submit_command(
                rocky_core::commitment("queued", &index.to_le_bytes()),
                world.domain,
                "Reserve",
                2,
                &world.reserve(1, &format!("queued-{index}")),
            )
            .await
            .unwrap();
    }
    let over = world.reserve(1, "over");
    assert!(matches!(
        executor
            .submit_command([9; 32], world.domain, "Reserve", 2, &over)
            .await,
        Err(Error::Backpressure)
    ));
    assert_eq!(executor.ticket([9; 32]).await.unwrap(), None);
    executor
        .cancel_ticket(rocky_core::commitment("queued", &0usize.to_le_bytes()))
        .await
        .unwrap();
    executor
        .submit_command([9; 32], world.domain, "Reserve", 2, &over)
        .await
        .unwrap();
}

#[tokio::test]
async fn scoped_results_stay_scoped_and_the_coordinator_sees_commitments_only() {
    let world = World::new();
    let executor = world.executor().await;
    let recording = Recording {
        inner: world.coordinator.clone(),
        proposals: Mutex::default(),
    };
    world.restock(&executor, 2).await;
    executor
        .submit_command(
            [1; 32],
            world.domain,
            "Reserve",
            2,
            &world.reserve(1, "private-hold"),
        )
        .await
        .unwrap();
    let ticket = executor.run_ticket([1; 32], &recording).await.unwrap();
    assert_eq!(ticket.custody, Vec::new());

    // What reached the coordinator names no descriptor, argument or row.
    let descriptor = world.app(None).commands().get("Reserve", 2).unwrap().id();
    let proposals = recording.proposals.lock().unwrap().clone();
    assert_eq!(proposals.len(), 1);
    for needle in [
        &descriptor[..],
        b"bolt",
        b"private-hold",
        b"Reserve",
        b"stock.reserve.v2",
    ] {
        assert!(
            !proposals[0]
                .windows(needle.len())
                .any(|window| window == needle)
        );
    }

    // A reader of the public scope gets and verifies its part alone.
    let reader_device = Device::new();
    let reader = world
        .open("reader.db", &reader_device, &[(world.stock, READ)], None)
        .await;
    let credentials = reader.inner.lock().await.config.credentials.clone();
    let part = async |sequence, scopes: &[ResourceScope]| {
        executor
            .encrypted_result_for(
                reader_device.node.public_key(),
                &credentials,
                world.domain,
                sequence,
                scopes,
            )
            .await
    };
    assert!(part(2, &[world.stock, world.holds]).await.is_err());
    assert!(part(2, &[world.holds]).await.is_err());
    let second = part(2, &[world.stock]).await.unwrap().unwrap();
    assert!(second.parts.iter().all(|part| part.scope == world.stock));
    PackageSet::verify_confirmed_subset(
        &second.strong_package().unwrap(),
        &second.receipt().unwrap(),
        &*world.coordinator,
        &second.parts,
    )
    .await
    .unwrap();
    // The receipt is valid; without its predecessor the state stays unavailable.
    reader
        .receive_encrypted_result(second.clone(), &*world.coordinator)
        .await
        .unwrap();
    let pending = reader.command_snapshot(world.stock).await.unwrap();
    assert!(!pending.complete && pending.rows.is_empty());
    let first = part(1, &[world.stock]).await.unwrap().unwrap();
    reader
        .receive_encrypted_result(first, &*world.coordinator)
        .await
        .unwrap();
    assert_eq!(world.remaining(&reader).await, 1);
    // Holding a receipt for the command grants nothing in the private scope.
    assert!(reader.command_snapshot(world.holds).await.is_err());
    assert!(
        reader
            .receive_encrypted_result(
                executor
                    .execute_encrypted(
                        executor.export_ticket([1; 32]).await.unwrap(),
                        &*world.coordinator
                    )
                    .await
                    .unwrap(),
                &*world.coordinator
            )
            .await
            .is_err()
    );

    // The reader's acknowledgment is custody evidence on the ticket.
    let states = reader.result_ack(&second).await.unwrap();
    executor
        .acknowledge_result(
            reader_device.node.public_key(),
            &credentials,
            &second,
            &states,
        )
        .await
        .unwrap();
    let ticket = executor.ticket([1; 32]).await.unwrap().unwrap();
    assert_eq!(
        ticket.custody,
        vec![Custody {
            peer: reader_device.node.public_key(),
            scope: world.stock,
            applied: true
        }]
    );

    // A caller that loses the result scope's key sees a confirmed ticket
    // whose result is unavailable, never an invented one.
    assert_eq!(ticket.state, TicketState::Applied);
    assert!(matches!(ticket.result, TicketResult::Available(_)));
    executor.evict_key(world.holds).await;
    let blind = executor.ticket([1; 32]).await.unwrap().unwrap();
    assert_eq!(blind.result, TicketResult::Unavailable);
    assert_eq!(blind.sequence, Some(2));
    assert!(matches!(
        blind.state,
        TicketState::Applied | TicketState::Confirmed
    ));
}

/// A delivered ticket whose identity the coordinator settled is never run
/// again; an acknowledgment retires the settled prefix.
#[tokio::test]
async fn settled_identities_end_tickets_and_acknowledgment_retires_them() {
    let world = World::new();
    let executor = world.executor().await;
    let other = Device::new();
    let caller = world.caller("caller.db", &other).await;
    world.restock(&executor, 2).await;
    caller
        .submit_command(
            [3; 32],
            world.domain,
            "Reserve",
            2,
            &world.reserve(1, "late"),
        )
        .await
        .unwrap();
    let envelope = caller.export_ticket([3; 32]).await.unwrap();
    // The command reaches the executor after its admission window closed.
    let now = world.coordinator.time().unwrap();
    world.coordinator.set_time(now + 86_400 + 60).unwrap();
    let answer = executor
        .deliver_command(&envelope.to_bytes().unwrap(), &*world.coordinator)
        .await;
    assert!(matches!(answer, Delivery::Settled { expired: true }));
    let answer = Delivery::from_bytes(&answer.to_bytes().unwrap()).unwrap();
    let settled = caller
        .settle_delivery([3; 32], answer, &*world.coordinator)
        .await
        .unwrap();
    assert_eq!(settled.state, TicketState::Expired);
    assert!(caller.open_tickets(8).await.unwrap().is_empty());
    drop(caller);
    let caller = world.caller("caller.db", &other).await;
    assert_eq!(
        caller.ticket([3; 32]).await.unwrap().unwrap().state,
        TicketState::Expired
    );
    let sequence = world.sequence();
    // Running it again neither executes nor fails.
    let rerun = executor
        .deliver_command(&envelope.to_bytes().unwrap(), &*world.coordinator)
        .await;
    assert!(matches!(rerun, Delivery::Settled { expired: true }));
    assert_eq!(world.sequence(), sequence);
    // Acknowledgment preserves a floor even without a confirmed response.
    caller
        .acknowledge_tickets(&*world.coordinator)
        .await
        .unwrap();
    assert_eq!(
        world
            .coordinator
            .status(world.domain, settled.request)
            .unwrap(),
        rocky_coordinator::Status::Retired
    );
    // The executor's applied restock is acknowledged and retired.
    let restock = executor
        .ticket(rocky_core::commitment("restock", &0u64.to_le_bytes()))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(restock.state, TicketState::Applied);
    assert!(matches!(
        world
            .coordinator
            .status(world.domain, restock.request)
            .unwrap(),
        rocky_coordinator::Status::Confirmed(_)
    ));
    executor
        .acknowledge_tickets(&*world.coordinator)
        .await
        .unwrap();
    assert_eq!(
        world
            .coordinator
            .status(world.domain, restock.request)
            .unwrap(),
        rocky_coordinator::Status::Retired
    );
}
