//! Independent draft notes and predecessor-based stock reservation.
use rocky_core::*;
use std::collections::BTreeMap;

pub const RESERVE: &str = "ReserveStock";
pub const INITIAL_STOCK: u64 = 10;

/// `ReserveStock` version 2: typed arguments and a typed outcome. Version 1
/// keeps its retained bytes, a little-endian `u64` quantity, and stays
/// inside the support horizon.
pub struct Reserve;
/// The quantity to reserve.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Quantity(pub u64);
/// What is left after an accepted reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Remaining(pub u64);
/// The stock that was too small for a rejected reservation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Shortfall(pub u64);
macro_rules! quantity_record {
    ($record:ident, $field:literal) => {
        impl Record for $record {
            fn fields() -> Vec<CommandField> {
                vec![CommandField::new($field, ValueKind::Integer, false)]
            }
            fn to_values(&self) -> Vec<SqlValue> {
                vec![SqlValue::Integer(self.0 as i64)]
            }
            fn from_values(values: Vec<SqlValue>) -> Option<Self> {
                match values.as_slice() {
                    [SqlValue::Integer(value)] => u64::try_from(*value).ok().map(Self),
                    _ => None,
                }
            }
        }
    };
}
quantity_record!(Quantity, "quantity");
quantity_record!(Remaining, "remaining");
quantity_record!(Shortfall, "remaining");
impl TypedCommand for Reserve {
    const NAME: &'static str = RESERVE;
    const VERSION: u32 = 2;
    type Arguments = Quantity;
    type Accepted = Remaining;
    type Rejected = Shortfall;
}
static COMMANDS: std::sync::LazyLock<CommandSet> = std::sync::LazyLock::new(|| {
    CommandDefinitions {
        format: COMMAND_FORMAT,
        handler: 1,
        commands: vec![Reserve::definition(
            "inventory.reserve.v2",
            "stock",
            "stock",
            &["stock"],
        )],
    }
    .build()
    .unwrap_or_default()
});
/// The typed descriptors of this application.
pub fn commands() -> &'static CommandSet {
    &COMMANDS
}
/// Canonical arguments of a version 2 reservation.
pub fn reserve(quantity: u64) -> Result<Vec<u8>, CommandError> {
    Reserve::encode_arguments(COMMANDS.get(RESERVE, 2)?, &Quantity(quantity))
}
#[derive(Clone, Copy)]
pub struct InventoryApp;
#[derive(Clone, Default)]
pub struct Drafts(BTreeMap<OperationId, (String, Stamp)>);
#[derive(Clone)]
pub struct Policy {
    pub scope: Scope,
    pub caller: PrincipalId,
    pub reference: Commitment,
    pub note_prefix: Option<String>,
}
#[derive(Debug)]
pub struct Invalid;
impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid inventory transition or authorization")
    }
}
impl std::error::Error for Invalid {}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Reservation {
    pub remaining: u64,
    pub accepted: bool,
    pub jobs: Vec<(RequestId, u64)>,
}
impl Reservation {
    pub fn decode(bytes: &[u8]) -> Result<Self, Invalid> {
        if bytes.len() < 13 || bytes[8] > 1 {
            return Err(Invalid);
        }
        let remaining = u64::from_le_bytes(bytes[..8].try_into().map_err(|_| Invalid)?);
        let count = u32::from_le_bytes(bytes[9..13].try_into().map_err(|_| Invalid)?) as usize;
        if count > 256 || bytes.len() != 13 + count * 32 || remaining > INITIAL_STOCK {
            return Err(Invalid);
        }
        let mut jobs = Vec::new();
        for job in bytes[13..].as_chunks::<32>().0 {
            let id = RequestId {
                node: NodeId(job[..16].try_into().map_err(|_| Invalid)?),
                sequence: AuthorSequence::new(u64::from_le_bytes(
                    job[16..24].try_into().map_err(|_| Invalid)?,
                ))
                .map_err(|_| Invalid)?,
            };
            let quantity = u64::from_le_bytes(job[24..].try_into().map_err(|_| Invalid)?);
            if quantity == 0 || jobs.iter().any(|(prior, _)| *prior == id) {
                return Err(Invalid);
            }
            jobs.push((id, quantity));
        }
        let used = jobs
            .iter()
            .try_fold(0u64, |sum, (_, amount)| sum.checked_add(*amount))
            .ok_or(Invalid)?;
        if used.checked_add(remaining) != Some(INITIAL_STOCK) {
            return Err(Invalid);
        }
        Ok(Self {
            remaining,
            accepted: bytes[8] == 1,
            jobs,
        })
    }
    /// The typed outcome of the request this result was confirmed for.
    pub fn outcome(&self) -> Result<Remaining, Shortfall> {
        if self.accepted {
            Ok(Remaining(self.remaining))
        } else {
            Err(Shortfall(self.remaining))
        }
    }
    fn encode(&self) -> Vec<u8> {
        let mut bytes = self.remaining.to_le_bytes().to_vec();
        bytes.push(u8::from(self.accepted));
        bytes.extend((self.jobs.len() as u32).to_le_bytes());
        for (id, amount) in &self.jobs {
            bytes.extend(id.node.0);
            bytes.extend(id.sequence.get().to_le_bytes());
            bytes.extend(amount.to_le_bytes());
        }
        bytes
    }
}
pub fn app_id() -> AppId {
    ApplicationManifest::app_id([0x81; 32], "inventory")
}
pub fn manifest() -> ApplicationManifest {
    let mut manifest = ApplicationManifest {
        publisher: [0x81; 32],
        name: "inventory".into(),
        app: app_id(),
        schema: 1,
        projection: 1,
        handler: 1,
        entities: Vec::new(),
        fields: Vec::new(),
        methods: Vec::new(),
        operations: Vec::new(),
        domains: [DomainId([1; 16]), DomainId([2; 16])]
            .into_iter()
            .map(|domain| DomainRule {
                domain,
                author_log: AuthorLogScope::Domain,
                rule: "independent stock=10; domain-scoped author logs; no cross-domain references"
                    .into(),
            })
            .collect(),
    };
    for (kind, name, shape) in [
        ("entity", "Draft", "operation-id"),
        ("entity", "Stock", "genesis quantity=10"),
        ("field", "Note", "UTF-8<=1024; LWW lamport/node/sequence"),
        (
            "method",
            RESERVE,
            "v1 u64-le quantity>0; reject insufficient stock; retain fulfillment job per accepted request",
        ),
        (
            "operation",
            "SetNote",
            "UTF-8<=1024; create or LWW edit; no stock mutation",
        ),
    ] {
        let mut item = DefinitionItem {
            id: manifest.item_id(kind, name),
            name: name.into(),
            shape: shape.into(),
        };
        // The signed definition binds the typed descriptor of version 2.
        if kind == "method"
            && let Some(typed) = commands().methods(manifest.publisher, manifest.app).first()
        {
            item.shape = format!("{}; v2 {}", item.shape, typed.shape);
        }
        match kind {
            "entity" => manifest.entities.push(item),
            "field" => manifest.fields.push(item),
            "method" => manifest.methods.push(item),
            _ => manifest.operations.push(item),
        }
    }
    manifest
}
impl Application for InventoryApp {
    type State = Drafts;
    type Error = Invalid;
    type Policy = Policy;
    fn validate(&self, op: &Envelope) -> Result<(), Invalid> {
        op.validate_dependencies(op.entity == op.id)
            .map_err(|_| Invalid)?;
        if op.payload.len() > 1024 || String::from_utf8(op.payload.clone()).is_err() {
            return Err(Invalid);
        }
        Ok(())
    }
    fn admit(&self, op: &Envelope, policy: &Policy) -> Result<(), Invalid> {
        self.admit_read(op.scope, op.caller, policy)
    }
    fn apply_ready(
        &self,
        state: &mut Drafts,
        op: &Envelope,
        _: AdmissionBoundary,
    ) -> Result<(), Invalid> {
        Application::validate(self, op)?;
        let stamp = Stamp::from(op);
        if state.0.get(&op.entity).is_none_or(|(_, old)| stamp > *old) {
            state.0.insert(
                op.entity,
                (
                    String::from_utf8(op.payload.clone()).map_err(|_| Invalid)?,
                    stamp,
                ),
            );
        }
        Ok(())
    }
}
fn draft_row(id: OperationId, note: &str, stamp: Stamp) -> Vec<SqlValue> {
    vec![
        SqlValue::Blob(id.node.0.to_vec()),
        SqlValue::Integer(id.sequence.get() as i64),
        SqlValue::Text(note.into()),
        SqlValue::Integer(stamp.lamport.get() as i64),
        SqlValue::Blob(stamp.node.0.to_vec()),
        SqlValue::Integer(stamp.sequence.get() as i64),
    ]
}
impl CompiledSchema for InventoryApp {
    fn schema(&self) -> &'static [&'static str] {
        &[
            "CREATE TABLE drafts(node BLOB,sequence INTEGER,note TEXT,lamport INTEGER,stamp_node BLOB,stamp_sequence INTEGER,PRIMARY KEY(node,sequence))",
            "CREATE TABLE stock(id INTEGER PRIMARY KEY,remaining INTEGER NOT NULL,accepted INTEGER NOT NULL)",
            "CREATE TABLE jobs(node BLOB,sequence INTEGER,quantity INTEGER,PRIMARY KEY(node,sequence))",
        ]
    }
    fn projection_query(&self) -> &'static str {
        "SELECT node,sequence,note,lamport,stamp_node,stamp_sequence FROM drafts ORDER BY node,sequence"
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        &["drafts"]
    }
}
impl StoredApplication for InventoryApp {
    type Row = (OperationId, String);
    fn descriptor(&self) -> ApplicationDescriptor {
        ApplicationDescriptor {
            definition: "inventory.v1",
            operations: Versions::V1,
            projection: 1,
        }
    }
    fn manifest(&self) -> Option<ApplicationManifest> {
        Some(manifest())
    }
    fn commands(&self) -> &'static [CommandMethod] {
        &[
            CommandMethod {
                name: RESERVE,
                version: 1,
                schema: 1,
                handler: 1,
            },
            CommandMethod {
                name: RESERVE,
                version: 2,
                schema: 1,
                handler: 1,
            },
        ]
    }
    fn command(&self, method: &str) -> Option<&dyn CommandHandler<Self>> {
        (method == RESERVE).then_some(self)
    }
    fn projection_rows(&self, state: &Drafts) -> Vec<Vec<SqlValue>> {
        state
            .0
            .iter()
            .map(|(id, (note, stamp))| draft_row(*id, note, *stamp))
            .collect()
    }
    fn touched(&self, op: &Envelope) -> Vec<ProjectionStatement> {
        vec![ProjectionStatement {
            sql: "SELECT node,sequence,note,lamport,stamp_node,stamp_sequence FROM drafts WHERE node=? AND sequence=?".into(),
            values: vec![
                SqlValue::Blob(op.entity.node.0.to_vec()),
                SqlValue::Integer(op.entity.sequence.get() as i64),
            ],
        }]
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Drafts, Invalid> {
        let mut drafts = Drafts::default();
        for row in rows {
            let [
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
                SqlValue::Text(note),
                SqlValue::Integer(lamport),
                SqlValue::Blob(stamp_node),
                SqlValue::Integer(stamp_sequence),
            ] = row.as_slice()
            else {
                return Err(Invalid);
            };
            let node_id = |bytes: &[u8]| bytes.try_into().map(NodeId).map_err(|_| Invalid);
            let counter = |value: i64| AuthorSequence::new(value as u64).map_err(|_| Invalid);
            drafts.0.insert(
                OperationId {
                    node: node_id(node)?,
                    sequence: counter(*sequence)?,
                },
                (
                    note.clone(),
                    Stamp {
                        lamport: LamportTime::new(*lamport as u64).map_err(|_| Invalid)?,
                        node: node_id(stamp_node)?,
                        sequence: counter(*stamp_sequence)?,
                    },
                ),
            );
        }
        Ok(drafts)
    }
    fn project(&self, state: &Drafts, op: &Envelope) -> Vec<ProjectionStatement> {
        let (note, stamp) = &state.0[&op.entity];
        vec![ProjectionStatement {
            sql: "INSERT INTO drafts VALUES(?,?,?,?,?,?) ON CONFLICT(node,sequence) DO UPDATE SET note=excluded.note,lamport=excluded.lamport,stamp_node=excluded.stamp_node,stamp_sequence=excluded.stamp_sequence".into(),
            values: draft_row(op.entity, note, *stamp),
        }]
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Invalid> {
        rows.into_iter()
            .map(|row| {
                let [
                    SqlValue::Blob(node),
                    SqlValue::Integer(sequence),
                    SqlValue::Text(note),
                    _,
                    _,
                    _,
                ] = row.as_slice()
                else {
                    return Err(Invalid);
                };
                Ok((
                    OperationId {
                        node: NodeId(node.as_slice().try_into().map_err(|_| Invalid)?),
                        sequence: AuthorSequence::new(*sequence as u64).map_err(|_| Invalid)?,
                    },
                    note.clone(),
                ))
            })
            .collect()
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Policy,
    ) -> Result<(), Invalid> {
        if scope != policy.scope || caller != policy.caller {
            return Err(Invalid);
        }
        Ok(())
    }
    fn select_rows(
        &self,
        rows: Vec<Self::Row>,
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<Vec<Self::Row>, Invalid> {
        self.admit_read(context.scope, context.caller, policy)?;
        Ok(rows
            .into_iter()
            .filter(|(_, note)| {
                policy
                    .note_prefix
                    .as_ref()
                    .is_none_or(|prefix| note.starts_with(prefix))
            })
            .collect())
    }
    fn read_views(
        &self,
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<Vec<ReadView>, Invalid> {
        self.admit_read(context.scope, context.caller, policy)?;
        let drafts = match &policy.note_prefix {
            // Exact prefix match; LIKE would fold case and read wildcards.
            Some(prefix) => ReadView {
                name: "drafts".into(),
                sql: "SELECT node,sequence,note FROM drafts WHERE substr(note,1,length(?))=?"
                    .into(),
                values: vec![SqlValue::Text(prefix.clone()); 2],
                key: rocky_core::columns(&["node", "sequence"]),
                not_null: rocky_core::columns(&[]),
            },
            None => ReadView {
                name: "drafts".into(),
                sql: "SELECT node,sequence,note FROM drafts".into(),
                values: Vec::new(),
                key: rocky_core::columns(&["node", "sequence"]),
                not_null: rocky_core::columns(&[]),
            },
        };
        Ok(vec![
            drafts,
            // No confirmed reservation yet means the genesis stock.
            ReadView {
                name: "stock".into(),
                sql: "SELECT remaining,accepted FROM stock UNION ALL SELECT 10,0 WHERE NOT EXISTS (SELECT 1 FROM stock)".into(),
                values: Vec::new(),
                // One row: nothing to page.
                ..ReadView::default()
            },
            ReadView {
                name: "jobs".into(),
                sql: "SELECT node,sequence,quantity FROM jobs".into(),
                values: Vec::new(),
                key: rocky_core::columns(&["node", "sequence"]),
                not_null: rocky_core::columns(&[]),
            },
        ])
    }
}
/// The requested quantity, read as the request's version encodes it. No
/// version is translated into another.
fn quantity(request: &StrongRequest) -> Result<u64, Invalid> {
    match request.version {
        1 => Ok(u64::from_le_bytes(
            request
                .arguments
                .as_slice()
                .try_into()
                .map_err(|_| Invalid)?,
        )),
        2 => {
            let descriptor = commands().get(RESERVE, 2).map_err(|_| Invalid)?;
            Ok(Reserve::decode_arguments(descriptor, &request.arguments)
                .map_err(|_| Invalid)?
                .0)
        }
        _ => Err(Invalid),
    }
}
impl CommandHandler<InventoryApp> for InventoryApp {
    fn seal(&self, state: &Drafts) -> Result<Vec<u8>, Invalid> {
        let mut bytes = Vec::new();
        for (id, (note, stamp)) in &state.0 {
            bytes.extend(id.node.0);
            bytes.extend(id.sequence.get().to_le_bytes());
            bytes.extend(stamp.lamport.get().to_le_bytes());
            bytes.extend(stamp.node.0);
            bytes.extend(stamp.sequence.get().to_le_bytes());
            bytes.extend((note.len() as u32).to_le_bytes());
            bytes.extend(note.as_bytes());
        }
        Ok(bytes)
    }
    fn admit(&self, request: &StrongRequest, policy: &Policy) -> Result<(), Invalid> {
        self.admit_read(request.scope, request.caller, policy)?;
        if request.policy != policy.reference {
            return Err(Invalid);
        }
        Ok(())
    }
    fn select_result(
        &self,
        bytes: &[u8],
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<bool, Invalid> {
        self.admit_read(context.scope, context.caller, policy)?;
        Reservation::decode(bytes)?;
        Ok(true)
    }
    fn validate(&self, request: &StrongRequest) -> Result<(), Invalid> {
        if request.method != RESERVE
            || request.schema != 1
            || request.handler != 1
            || quantity(request)? == 0
        {
            return Err(Invalid);
        }
        Ok(())
    }
    fn execute(&self, state: &Drafts, request: &StrongRequest) -> Result<Vec<u8>, Invalid> {
        self.execute_after(state, request, None)
    }
    fn execute_after(
        &self,
        _: &Drafts,
        request: &StrongRequest,
        previous: Option<&[u8]>,
    ) -> Result<Vec<u8>, Invalid> {
        <Self as CommandHandler<Self>>::validate(self, request)?;
        let mut result = previous
            .map(Reservation::decode)
            .transpose()?
            .unwrap_or(Reservation {
                remaining: INITIAL_STOCK,
                accepted: false,
                jobs: Vec::new(),
            });
        let quantity = quantity(request)?;
        result.accepted = quantity <= result.remaining;
        if result.accepted {
            result.remaining -= quantity;
            result.jobs.push((request.id, quantity));
        }
        Ok(result.encode())
    }
    fn result_projection_query(&self) -> Option<&'static str> {
        Some(
            "SELECT 'stock',NULL,1,remaining,accepted FROM stock UNION ALL SELECT 'job',node,sequence,quantity,NULL FROM jobs ORDER BY 1,2,3",
        )
    }
    fn result_projection_rows(&self, bytes: &[u8]) -> Result<Vec<Vec<SqlValue>>, Invalid> {
        let result = Reservation::decode(bytes)?;
        let mut jobs = result.jobs;
        jobs.sort_by_key(|(id, _)| *id);
        let mut rows: Vec<_> = jobs
            .iter()
            .map(|(id, quantity)| {
                vec![
                    SqlValue::Text("job".into()),
                    SqlValue::Blob(id.node.0.to_vec()),
                    SqlValue::Integer(id.sequence.get() as i64),
                    SqlValue::Integer(*quantity as i64),
                    SqlValue::Null,
                ]
            })
            .collect();
        rows.push(vec![
            SqlValue::Text("stock".into()),
            SqlValue::Null,
            SqlValue::Integer(1),
            SqlValue::Integer(result.remaining as i64),
            SqlValue::Integer(i64::from(result.accepted)),
        ]);
        Ok(rows)
    }
    fn project_result(&self, bytes: &[u8]) -> Result<Vec<ProjectionStatement>, Invalid> {
        let result = Reservation::decode(bytes)?;
        let mut statements = vec![ProjectionStatement {
            sql: "INSERT INTO stock VALUES(1,?,?) ON CONFLICT(id) DO UPDATE SET remaining=excluded.remaining,accepted=excluded.accepted".into(),
            values: vec![
                SqlValue::Integer(result.remaining as i64),
                SqlValue::Integer(i64::from(result.accepted)),
            ],
        }];
        statements.extend(
            result
                .jobs
                .iter()
                .map(|(id, quantity)| ProjectionStatement {
                    sql: "INSERT INTO jobs VALUES(?,?,?) ON CONFLICT(node,sequence) DO NOTHING"
                        .into(),
                    values: vec![
                        SqlValue::Blob(id.node.0.to_vec()),
                        SqlValue::Integer(id.sequence.get() as i64),
                        SqlValue::Integer(*quantity as i64),
                    ],
                }),
        );
        Ok(statements)
    }
}
