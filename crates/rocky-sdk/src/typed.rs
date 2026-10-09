//! The generated application over typed tables: business keys, row
//! incarnations and register columns. Protocol state lives in the private
//! `rocky_incarnations` table; each logical table is a projection of it.
//! See `docs/runtime/logical-schema.md`.
use rocky_core::{
    AdmissionBoundary, AdmissionContext, Affinity, Application, ApplicationDescriptor,
    ApplicationManifest, AuthorSequence, CapturedRow, ColumnClass, Commitment, DomainId, Envelope,
    InstalledSchema, Intent, KeyColumn, LamportTime, LogicalSchema, NodeId, OperationId,
    PrincipalId, ProjectionSchema, ProjectionStatement, ReadView, Scope, Sql, SqlValue, SqlWrites,
    Stamp, StoredApplication, UpdatePolicy, Versions, bind_rule, key_components, key_values,
};
use rocky_runtime::Mutation;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

/// Incarnations one insert may replace or one delete may observe.
pub const MAX_OBSERVED: usize = 16;
const DEFINITION: &str = "rocky.sdk.typed.v2";
const SCHEMA: [&str; 5] = [
    "CREATE TABLE rocky_incarnations(tbl BLOB NOT NULL,row_key BLOB NOT NULL,node BLOB NOT NULL,sequence INTEGER NOT NULL,tombstoned INTEGER NOT NULL,state BLOB,target_node BLOB,target_sequence INTEGER,PRIMARY KEY(tbl,row_key,node,sequence)) WITHOUT ROWID",
    "CREATE TABLE rocky_identity_marks(tbl BLOB NOT NULL,author INTEGER NOT NULL,sequence INTEGER NOT NULL,PRIMARY KEY(tbl,author)) WITHOUT ROWID",
    "CREATE INDEX rocky_incarnations_id ON rocky_incarnations(node,sequence)",
    "CREATE TABLE rocky_follow_fields(source_node BLOB NOT NULL,source_sequence INTEGER NOT NULL,original_node BLOB NOT NULL,original_sequence INTEGER NOT NULL,field BLOB NOT NULL,PRIMARY KEY(source_node,source_sequence,original_node,original_sequence,field)) WITHOUT ROWID",
    "CREATE INDEX rocky_follow_fields_author ON rocky_follow_fields(original_node,source_node,source_sequence)",
];
const SELECT_ALL: &str = "SELECT tbl,row_key,node,sequence,tombstoned,state FROM rocky_incarnations UNION ALL SELECT tbl,NULL,NULL,sequence,author,NULL FROM rocky_identity_marks ORDER BY tbl,row_key,node,sequence";
const LIVE_AT_KEY: &str = "SELECT tbl,row_key,node,sequence,tombstoned,state FROM rocky_incarnations WHERE tbl=? AND row_key=? AND tombstoned=0";
const ONE_AT_KEY: &str = "SELECT tbl,row_key,node,sequence,tombstoned,state FROM rocky_incarnations WHERE tbl=? AND row_key=? AND node=? AND sequence=?";
const ENTITY: &str = "SELECT tbl,row_key,node,sequence,tombstoned,state FROM rocky_incarnations WHERE node=? AND sequence=?";
/// Live incarnations at the key of incarnation `?1,?2`.
const LIVE_WITH: &str = "SELECT tbl,row_key,node,sequence,tombstoned,state FROM rocky_incarnations WHERE tombstoned=0 AND (tbl,row_key) IN (SELECT tbl,row_key FROM rocky_incarnations WHERE node=? AND sequence=?)";
/// Incarnation `?3,?4` at the key of incarnation `?1,?2`.
const ONE_WITH: &str = "SELECT tbl,row_key,node,sequence,tombstoned,state FROM rocky_incarnations WHERE node=?3 AND sequence=?4 AND (tbl,row_key) IN (SELECT tbl,row_key FROM rocky_incarnations WHERE node=?1 AND sequence=?2)";
const STORE: &str = "INSERT INTO rocky_incarnations(tbl,row_key,node,sequence,tombstoned,state) VALUES(?,?,?,?,?,?) ON CONFLICT(tbl,row_key,node,sequence) DO UPDATE SET tombstoned=excluded.tombstoned,state=excluded.state";

#[derive(Debug)]
pub struct TypedError(String);
impl std::fmt::Display for TypedError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for TypedError {}
fn refuse<T>(reason: impl Into<String>) -> Result<T, TypedError> {
    Err(TypedError(reason.into()))
}
fn failed(error: impl std::fmt::Display) -> TypedError {
    TypedError(error.to_string())
}

struct Field {
    id: Commitment,
    name: String,
    affinity: Affinity,
    not_null: bool,
    /// Position among the table's declared columns.
    position: usize,
}
struct Table {
    partitions: Vec<usize>,
    id: Commitment,
    name: String,
    key: Vec<KeyColumn>,
    key_names: Vec<String>,
    /// Positions of the key among the declared columns, in key order.
    key_positions: Vec<usize>,
    /// Declared columns, the width of a captured row.
    width: usize,
    /// Register columns, sorted by field identity.
    fields: Vec<Field>,
    upsert: String,
    delete: String,
}
struct Plan {
    schema: LogicalSchema,
    manifest: ApplicationManifest,
    installed: InstalledSchema,
    tables: Vec<Table>,
    views: Vec<ReadView>,
}
/// Trusted development admission, as in the register application.
#[derive(Clone)]
pub struct Access {
    pub scope: Scope,
    pub caller: PrincipalId,
    pub nodes: Vec<NodeId>,
}
#[derive(Clone)]
pub struct TypedApp(Arc<Plan>, Arc<BTreeMap<(NodeId, String), u32>>);

#[derive(Clone, PartialEq)]
struct Live {
    birth: Stamp,
    lamport: LamportTime,
    /// The key as this incarnation spelled it.
    key: Vec<SqlValue>,
    registers: Vec<(SqlValue, Stamp)>,
}
#[derive(Clone, PartialEq)]
enum Incarnation {
    Tombstone(Option<i64>),
    Live(Live),
    Moved(Moved),
}
type RowId = (Commitment, Vec<u8>, OperationId);
/// Register values by field identity, sorted.
type Assignments = Vec<(Commitment, SqlValue)>;
#[derive(Clone, Default)]
pub struct State(
    BTreeMap<RowId, Incarnation>,
    BTreeMap<(Commitment, u32), u32>,
);

/// A live incarnation. `visible` marks the one its typed table shows.
#[derive(Clone, Debug, PartialEq)]
pub struct TypedRow {
    pub table: String,
    pub incarnation: OperationId,
    pub identity: Option<rocky_core::RowIdentity>,
    pub visible: bool,
    pub key: Vec<SqlValue>,
    /// Register columns by SQL name.
    pub values: Vec<(String, SqlValue)>,
    pub stamps: Vec<(String, Stamp)>,
    pub birth: Stamp,
}
/// The incarnations a replica holds at one business key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Located {
    pub visible: Option<OperationId>,
    pub live: Vec<OperationId>,
}

enum Action {
    Move {
        table: Commitment,
        identity: i64,
        key: Vec<SqlValue>,
        values: Vec<(Commitment, SqlValue, Stamp)>,
        birth: Stamp,
    },
    Follow {
        table: Commitment,
        identity: i64,
        source: OperationId,
        set: Vec<(Commitment, SqlValue, Stamp)>,
    },
    Insert {
        table: Commitment,
        key: Vec<SqlValue>,
        values: Assignments,
        replaces: Vec<OperationId>,
    },
    Update {
        identity: i64,
        table: Commitment,
        set: Assignments,
    },
    Delete {
        identity: i64,
        table: Commitment,
        observed: Vec<OperationId>,
    },
}
fn put_ids(bytes: &mut Vec<u8>, ids: &[OperationId]) {
    bytes.push(ids.len() as u8);
    for id in ids {
        bytes.extend(id.node.0);
        bytes.extend(id.sequence.get().to_le_bytes());
    }
}
fn put_fields(bytes: &mut Vec<u8>, fields: &[(Commitment, SqlValue)]) {
    bytes.extend((fields.len() as u16).to_le_bytes());
    for (field, value) in fields {
        bytes.extend(field);
        value.encode(bytes);
    }
}
struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn array<const N: usize>(&mut self) -> Result<[u8; N], TypedError> {
        let (head, tail) = self
            .0
            .split_at_checked(N)
            .ok_or_else(|| TypedError("truncated".into()))?;
        self.0 = tail;
        let mut array = [0; N];
        array.copy_from_slice(head);
        Ok(array)
    }
    fn value(&mut self) -> Result<SqlValue, TypedError> {
        SqlValue::decode(&mut self.0).map_err(failed)
    }
    /// Strictly ascending, so one set has one encoding.
    fn ids(&mut self) -> Result<Vec<OperationId>, TypedError> {
        let count = usize::from(self.array::<1>()?[0]);
        let mut ids = Vec::with_capacity(count);
        for _ in 0..count {
            ids.push(OperationId {
                node: NodeId(self.array()?),
                sequence: AuthorSequence::new(u64::from_le_bytes(self.array()?)).map_err(failed)?,
            });
        }
        if count > MAX_OBSERVED || ids.windows(2).any(|pair| pair[0] >= pair[1]) {
            return refuse("incarnation list is not a bounded sorted set");
        }
        Ok(ids)
    }
    fn fields(&mut self) -> Result<Assignments, TypedError> {
        let count = u16::from_le_bytes(self.array()?);
        let mut fields = Vec::new();
        for _ in 0..count {
            fields.push((self.array()?, self.value()?));
        }
        if fields.windows(2).any(|pair| pair[0].0 >= pair[1].0) {
            return refuse("fields are not a sorted set");
        }
        Ok(fields)
    }
}
impl Action {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = Vec::new();
        match self {
            Self::Move {
                table,
                identity,
                key,
                values,
                birth,
            } => {
                bytes.push(6);
                bytes.extend(table);
                bytes.extend(identity.to_le_bytes());
                bytes.push(key.len() as u8);
                for value in key {
                    value.encode(&mut bytes);
                }
                stamp_bytes(&mut bytes, *birth);
                put_stamped(&mut bytes, values);
            }
            Self::Follow {
                table,
                identity,
                source,
                set,
            } => {
                bytes.push(7);
                bytes.extend(table);
                bytes.extend(identity.to_le_bytes());
                bytes.extend(source.node.0);
                bytes.extend(source.sequence.get().to_le_bytes());
                put_stamped(&mut bytes, set);
            }
            Self::Insert {
                table,
                key,
                values,
                replaces,
            } => {
                bytes.push(1);
                bytes.extend(table);
                bytes.push(key.len() as u8);
                for value in key {
                    value.encode(&mut bytes);
                }
                put_fields(&mut bytes, values);
                put_ids(&mut bytes, replaces);
            }
            Self::Update {
                table,
                set,
                identity,
            } => {
                bytes.push(4);
                bytes.extend(table);
                bytes.extend(identity.to_le_bytes());
                put_fields(&mut bytes, set);
            }
            Self::Delete {
                table,
                observed,
                identity,
            } => {
                bytes.push(5);
                bytes.extend(table);
                bytes.extend(identity.to_le_bytes());
                put_ids(&mut bytes, observed);
            }
        }
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self, TypedError> {
        let mut reader = Reader(bytes);
        let tag = reader.array::<1>()?[0];
        let table = reader.array()?;
        let action = match tag {
            6 => {
                let identity = i64::from_le_bytes(reader.array()?);
                let count = reader.array::<1>()?[0];
                let key = (0..count)
                    .map(|_| reader.value())
                    .collect::<Result<_, _>>()?;
                let birth = reader.stamp()?;
                Self::Move {
                    table,
                    identity,
                    key,
                    values: reader.stamped()?,
                    birth,
                }
            }
            7 => Self::Follow {
                table,
                identity: i64::from_le_bytes(reader.array()?),
                source: reader.operation()?,
                set: reader.stamped()?,
            },
            1 => {
                let count = reader.array::<1>()?[0];
                let key = (0..count)
                    .map(|_| reader.value())
                    .collect::<Result<_, _>>()?;
                Self::Insert {
                    table,
                    key,
                    values: reader.fields()?,
                    replaces: reader.ids()?,
                }
            }
            4 => Self::Update {
                identity: i64::from_le_bytes(reader.array()?),
                table,
                set: reader.fields()?,
            },
            5 => Self::Delete {
                identity: i64::from_le_bytes(reader.array()?),
                table,
                observed: reader.ids()?,
            },
            _ => return refuse("unknown action"),
        };
        if !reader.0.is_empty() {
            return refuse("trailing action bytes");
        }
        Ok(action)
    }
}

fn id_values(id: OperationId) -> [SqlValue; 2] {
    [
        SqlValue::Blob(id.node.0.to_vec()),
        SqlValue::Integer(id.sequence.get() as i64),
    ]
}
fn statement(sql: &'static str, values: Vec<SqlValue>) -> ProjectionStatement {
    ProjectionStatement {
        sql: Sql::Borrowed(sql),
        values,
    }
}
fn quoted(name: &str) -> String {
    format!("\"{name}\"")
}
fn creation(_id: OperationId, live: &Live) -> Stamp {
    live.birth
}
fn stamp_bytes(bytes: &mut Vec<u8>, stamp: Stamp) {
    bytes.extend(stamp.lamport.get().to_le_bytes());
    bytes.extend(stamp.node.0);
    bytes.extend(stamp.sequence.get().to_le_bytes());
}
impl Live {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![1];
        bytes.extend(self.lamport.get().to_le_bytes());
        stamp_bytes(&mut bytes, self.birth);
        bytes.push(self.key.len() as u8);
        for value in &self.key {
            value.encode(&mut bytes);
        }
        bytes.extend((self.registers.len() as u16).to_le_bytes());
        for (value, stamp) in &self.registers {
            value.encode(&mut bytes);
            stamp_bytes(&mut bytes, *stamp);
        }
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self, TypedError> {
        let mut reader = Reader(bytes);
        if reader.array::<1>()?[0] != 1 {
            return refuse("unknown incarnation state");
        }
        let lamport = LamportTime::new(u64::from_le_bytes(reader.array()?)).map_err(failed)?;
        let birth = reader.stamp()?;
        let count = reader.array::<1>()?[0];
        let key = (0..count)
            .map(|_| reader.value())
            .collect::<Result<_, _>>()?;
        let count = u16::from_le_bytes(reader.array()?);
        let mut registers = Vec::new();
        for _ in 0..count {
            let value = reader.value()?;
            let stamp = Stamp {
                lamport: LamportTime::new(u64::from_le_bytes(reader.array()?)).map_err(failed)?,
                node: NodeId(reader.array()?),
                sequence: AuthorSequence::new(u64::from_le_bytes(reader.array()?))
                    .map_err(failed)?,
            };
            registers.push((value, stamp));
        }
        if !reader.0.is_empty() {
            return refuse("trailing incarnation state");
        }
        Ok(Self {
            birth,
            lamport,
            key,
            registers,
        })
    }
}
impl State {
    /// Live incarnations at one key, with their creation stamps.
    fn live<'a>(
        &'a self,
        table: &'a Commitment,
        key: &'a [u8],
    ) -> impl Iterator<Item = (OperationId, &'a Live)> {
        self.0
            .iter()
            .filter_map(move |((tbl, row, id), entry)| match entry {
                Incarnation::Live(live) if tbl == table && row == key => Some((*id, live)),
                _ => None,
            })
    }
    fn visible(&self, table: &Commitment, key: &[u8]) -> Option<(OperationId, &Live)> {
        self.0
            .iter()
            .filter_map(|((tbl, row, id), entry)| match entry {
                Incarnation::Live(live) if tbl == table && row == key => Some((*id, live)),
                _ => None,
            })
            .max_by_key(|(id, live)| creation(*id, live))
    }
    /// The row an update or delete names. A tombstone recorded ahead of an
    /// insert can share the identity at another key; the live row is the one.
    fn entity(&self, id: OperationId) -> Option<&RowId> {
        let mut rows = self.0.iter().filter(|((_, _, row), _)| *row == id);
        let first = rows.next()?;
        Some(
            std::iter::once(first)
                .chain(rows)
                .find(|(_, entry)| matches!(entry, Incarnation::Live(_)))
                .unwrap_or(first)
                .0,
        )
    }
}

impl Table {
    fn field(&self, id: &Commitment) -> Option<usize> {
        self.fields.iter().position(|field| field.id == *id)
    }
    /// The value exactly as the column stores it, or a refusal.
    fn stored(field: &Field, value: &SqlValue) -> Result<(), TypedError> {
        let stored = field.affinity.store(value.clone()).map_err(failed)?;
        if stored != *value || (field.not_null && stored == SqlValue::Null) {
            return refuse(format!("{} cannot store this value", field.name));
        }
        Ok(())
    }
    fn allocation(&self, values: &[SqlValue]) -> Result<Commitment, TypedError> {
        if values.len() != self.partitions.len() {
            return refuse("partition value count for identity allocation");
        }
        if !self.partitions.is_empty() {
            let columns: Vec<_> = self
                .partitions
                .iter()
                .map(|position| self.key[*position])
                .collect();
            key_components(&columns, values).map_err(failed)?;
        }
        Ok(self.id)
    }

    fn allocation_at(&self, key: &[SqlValue]) -> Result<Commitment, TypedError> {
        self.allocation(
            &self
                .partitions
                .iter()
                .map(|position| key[*position].clone())
                .collect::<Vec<_>>(),
        )
    }
    fn components(&self, key: &[SqlValue]) -> Result<Vec<u8>, TypedError> {
        key_components(&self.key, key).map_err(failed)
    }
}
impl TypedApp {
    /// Refuses a schema this runtime cannot keep convergent.
    pub fn new(schema: LogicalSchema, domains: &[DomainId]) -> Result<Self, TypedError> {
        schema
            .supported_on(rocky_core::Profile::Trusted)
            .map_err(failed)?;
        let manifest = schema.manifest(domains);
        manifest.validate().map_err(failed)?;
        let mut statements: Vec<Sql> = SCHEMA.into_iter().map(Sql::Borrowed).collect();
        let mut checkpoint = vec![
            Sql::Borrowed("rocky_incarnations"),
            Sql::Borrowed("rocky_identity_marks"),
            Sql::Borrowed("rocky_follow_fields"),
        ];
        let (mut tables, mut views) = (Vec::new(), Vec::new());
        statements.extend(
            schema
                .views
                .iter()
                .map(|view| view.create_sql.clone().into()),
        );
        for table in &schema.tables {
            statements.push(table.create_sql.clone().into());
            statements.extend(
                table
                    .indexes
                    .iter()
                    .map(|index| index.create_sql.clone().into()),
            );
            let names = |columns: &mut dyn Iterator<Item = &rocky_core::LogicalColumn>| {
                columns
                    .map(|column| Sql::Owned(column.sql_name.clone()))
                    .collect::<Vec<_>>()
            };
            let keyed = table.key().all(|column| column.not_null)
                && (1..=16).contains(&table.primary_key.len());
            let selected: Vec<_> = table.columns.iter().map(|c| quoted(&c.sql_name)).collect();
            views.push(ReadView {
                name: table.sql_name.clone().into(),
                sql: format!(
                    "SELECT {} FROM {}",
                    selected.join(","),
                    quoted(&table.sql_name)
                )
                .into(),
                values: Vec::new(),
                key: if keyed {
                    names(&mut table.key())
                } else {
                    Vec::new()
                },
                not_null: names(&mut table.columns.iter().filter(|column| column.not_null)),
            });
            if table.class != rocky_core::TableClass::Mergeable {
                continue;
            }
            checkpoint.push(table.sql_name.clone().into());
            let position = |name: &str| {
                table
                    .columns
                    .iter()
                    .position(|column| column.sql_name == name)
                    .unwrap_or(usize::MAX)
            };
            let mut fields: Vec<_> = table
                .registers()
                .map(|column| Field {
                    id: schema.field_id(table, column),
                    name: column.sql_name.clone(),
                    affinity: column.affinity,
                    not_null: column.not_null,
                    position: position(&column.sql_name),
                })
                .collect();
            fields.sort_by_key(|field| field.id);
            let key_names: Vec<String> = table.primary_key.clone();
            // Parameters: the key in key order, then the registers.
            let bound: Vec<&str> = key_names
                .iter()
                .map(String::as_str)
                .chain(fields.iter().map(|field| field.name.as_str()))
                .collect();
            let mut columns: Vec<String> = bound.iter().map(|name| quoted(name)).collect();
            let mut values: Vec<String> = (1..=bound.len()).map(|n| format!("?{n}")).collect();
            for column in &table.columns {
                if let ColumnClass::Derived { rule } = &column.class {
                    let parameter = |name: &str| {
                        bound
                            .iter()
                            .position(|known| *known == name)
                            .map(|position| format!("?{}", position + 1))
                    };
                    let rule = bind_rule(rule, parameter).ok_or_else(|| {
                        TypedError(format!("derived rule of {}", column.sql_name))
                    })?;
                    columns.push(quoted(&column.sql_name));
                    values.push(format!("({rule})"));
                }
            }
            let keys: Vec<String> = key_names.iter().map(|name| quoted(name)).collect();
            let assignments: Vec<String> = columns
                .iter()
                .map(|column| format!("{column}=excluded.{column}"))
                .collect();
            let conditions: Vec<String> = keys.iter().map(|key| format!("{key}=?")).collect();
            tables.push(Table {
                id: schema.table_id(table),
                upsert: format!(
                    "INSERT INTO {}({}) VALUES({}) ON CONFLICT({}) DO UPDATE SET {}",
                    quoted(&table.sql_name),
                    columns.join(","),
                    values.join(","),
                    keys.join(","),
                    assignments.join(",")
                ),
                delete: format!(
                    "DELETE FROM {} WHERE {}",
                    quoted(&table.sql_name),
                    conditions.join(" AND ")
                ),
                name: table.sql_name.clone(),
                key: table
                    .key()
                    .map(|column| KeyColumn {
                        affinity: column.affinity,
                        collation: column.collation,
                    })
                    .collect(),
                partitions: if table.partition_by == ["id"] {
                    vec![]
                } else {
                    table
                        .partition_by
                        .iter()
                        .filter_map(|name| key_names.iter().position(|key| key == name))
                        .collect()
                },
                key_positions: key_names.iter().map(|name| position(name)).collect(),
                width: table.columns.len(),
                key_names,
                fields,
            });
        }
        for view in &schema.views {
            views.push(ReadView {
                name: view.name.clone().into(),
                sql: rocky_storage::partition_sql::check_with_views(
                    &format!("SELECT * FROM {}", quoted(&view.name)),
                    &[],
                    &Default::default(),
                    &schema.views,
                )
                .map_err(failed)?
                .into(),
                values: Vec::new(),
                key: Vec::new(),
                not_null: Vec::new(),
            });
        }
        Ok(Self(
            Arc::new(Plan {
                installed: InstalledSchema {
                    statements,
                    additive_indexes: Vec::new(),
                    projection_query: Sql::Borrowed(SELECT_ALL),
                    checkpoint_tables: checkpoint,
                },
                manifest,
                schema,
                tables,
                views,
            }),
            Arc::default(),
        ))
    }
    /// Trusted installations bind each table's row author to its admitted node.
    pub fn with_row_author(
        mut self,
        node: NodeId,
        table: &str,
        author: u32,
    ) -> Result<Self, TypedError> {
        rocky_core::RowIdentity::new(author, 1).map_err(failed)?;
        self.by_name(table)?;
        let mut authors = (*self.1).clone();
        if authors
            .iter()
            .any(|((held, name), number)| name == table && *number == author && *held != node)
        {
            return refuse("row author belongs to another node");
        }
        authors.insert((node, table.into()), author);
        self.1 = Arc::new(authors);
        Ok(self)
    }
    fn mark_query(&self, table: Commitment, author: u32) -> ProjectionStatement {
        statement(
            "SELECT tbl,author,sequence FROM rocky_identity_marks WHERE tbl=? AND author=?",
            vec![
                SqlValue::Blob(table.to_vec()),
                SqlValue::Integer(i64::from(author)),
            ],
        )
    }
    pub fn schema(&self) -> &LogicalSchema {
        &self.0.schema
    }
    fn by_id(&self, id: &Commitment) -> Result<&Table, TypedError> {
        self.0
            .tables
            .iter()
            .find(|table| table.id == *id)
            .ok_or_else(|| TypedError("unknown table".into()))
    }
    fn by_name(&self, name: &str) -> Result<&Table, TypedError> {
        self.0
            .tables
            .iter()
            .find(|table| table.name == name)
            .ok_or_else(|| TypedError(format!("{name} is not a mergeable table")))
    }
    /// Decodes and checks an operation against the installed definition.
    fn action(&self, envelope: &Envelope) -> Result<Action, TypedError> {
        let action = Action::decode(&envelope.payload)?;
        envelope
            .validate_dependencies(matches!(action, Action::Insert { .. }))
            .map_err(failed)?;
        match &action {
            Action::Move {
                table,
                identity,
                key,
                values,
                birth,
            } => {
                let plan = self.by_id(table)?;
                let id = rocky_core::RowIdentity::from_integer(*identity).map_err(failed)?;
                if plan.partitions.is_empty()
                    || plan
                        .partitions
                        .iter()
                        .any(|position| plan.key_names[*position] == "id")
                {
                    return refuse("table cannot move");
                }
                plan.components(key)?;
                if *birth > Stamp::from(envelope)
                    || self.1.get(&(birth.node, plan.name.clone())) != Some(&id.author())
                    || values.len() != plan.fields.len()
                {
                    return refuse("move creation proof or field width");
                }
                for (field, (id, value, stamp)) in plan.fields.iter().zip(values) {
                    if field.id != *id || *stamp > Stamp::from(envelope) {
                        return refuse("move stamp or field ordering");
                    }
                    Table::stored(field, value)?;
                }
            }
            Action::Follow {
                table,
                identity,
                set,
                ..
            } => {
                let plan = self.by_id(table)?;
                rocky_core::RowIdentity::from_integer(*identity).map_err(failed)?;
                if set.is_empty() {
                    return refuse("follow sets nothing");
                }
                for (id, value, stamp) in set {
                    let field = plan
                        .field(id)
                        .ok_or_else(|| TypedError("unknown follow field".into()))?;
                    if ["id", "row_kind"].contains(&plan.fields[field].name.as_str())
                        || stamp.node != envelope.id.node
                        || *stamp > Stamp::from(envelope)
                    {
                        return refuse("follow is not signed by its original author");
                    }
                    Table::stored(&plan.fields[field], value)?;
                }
            }
            Action::Insert {
                table,
                key,
                values,
                replaces,
            } => {
                let table = self.by_id(table)?;
                table.components(key)?;
                {
                    let value = |name: &str| {
                        table
                            .key_names
                            .iter()
                            .position(|key| key == name)
                            .and_then(|index| key.get(index))
                            .or_else(|| {
                                table
                                    .fields
                                    .iter()
                                    .position(|field| field.name == name)
                                    .and_then(|index| values.get(index).map(|(_, value)| value))
                            })
                    };
                    let Some(SqlValue::Integer(id)) = value("id") else {
                        return refuse("missing row identity");
                    };
                    rocky_core::RowIdentity::from_integer(*id).map_err(failed)?;
                    if value("row_kind") != Some(&SqlValue::Integer(0)) {
                        return refuse("application rows require row_kind = 0");
                    }
                }
                // The key must be in stored form, so one key has one encoding.
                for (column, value) in table.key.iter().zip(key) {
                    if column.affinity.store(value.clone()).map_err(failed)? != *value {
                        return refuse("key is not in stored form");
                    }
                }
                if values.len() != table.fields.len() || replaces.contains(&envelope.id) {
                    return refuse("insert must set every register once");
                }
                for (field, (id, value)) in table.fields.iter().zip(values) {
                    if field.id != *id {
                        return refuse("insert must set every register once");
                    }
                    Table::stored(field, value)?;
                }
            }
            Action::Update {
                table,
                set,
                identity,
            } => {
                let table = self.by_id(table)?;
                rocky_core::RowIdentity::from_integer(*identity).map_err(failed)?;
                if set.is_empty() {
                    return refuse("update sets nothing");
                }
                for (id, value) in set {
                    let field = table
                        .field(id)
                        .ok_or_else(|| TypedError("unknown field".into()))?;
                    if ["id", "row_kind"].contains(&table.fields[field].name.as_str()) {
                        return refuse("row id and row_kind are immutable");
                    }
                    Table::stored(&table.fields[field], value)?;
                }
            }
            Action::Delete {
                table,
                observed,
                identity,
            } => {
                self.by_id(table)?;
                rocky_core::RowIdentity::from_integer(*identity).map_err(failed)?;
                if observed.contains(&envelope.entity) {
                    return refuse("delete observes its own target");
                }
            }
        }
        Ok(action)
    }
    fn mutation(entity: Option<OperationId>, action: Action) -> Mutation {
        Mutation {
            entity,
            payload: action.encode(),
        }
    }
    fn ids(ids: &[OperationId]) -> Result<Vec<OperationId>, TypedError> {
        let ids: BTreeSet<_> = ids.iter().copied().collect();
        if ids.len() > MAX_OBSERVED {
            return refuse("too many incarnations at one key");
        }
        Ok(ids.into_iter().collect())
    }
    /// The stored form of named column values, in field order.
    fn assignments(table: &Table, values: &[(&str, SqlValue)]) -> Result<Assignments, TypedError> {
        let mut set = BTreeMap::new();
        for (name, value) in values {
            let field = table
                .fields
                .iter()
                .find(|field| field.name == *name)
                .ok_or_else(|| TypedError(format!("{name} is not a register column")))?;
            let value = field.affinity.store(value.clone()).map_err(failed)?;
            Table::stored(field, &value)?;
            if set.insert(field.id, value).is_some() {
                return refuse(format!("{name} is assigned twice"));
            }
        }
        Ok(set.into_iter().collect())
    }
    /// Splits a full row into its stored key and register values.
    fn split(
        table: &Table,
        row: &[(&str, SqlValue)],
    ) -> Result<(Vec<SqlValue>, Assignments), TypedError> {
        let mut key = Vec::new();
        for (name, column) in table.key_names.iter().zip(&table.key) {
            let value = row
                .iter()
                .find(|(column, _)| column == name)
                .ok_or_else(|| TypedError(format!("missing key column {name}")))?;
            key.push(column.affinity.store(value.1.clone()).map_err(failed)?);
        }
        let rest: Vec<_> = row
            .iter()
            .filter(|(name, _)| !table.key_names.iter().any(|key| key == name))
            .cloned()
            .collect();
        let values = Self::assignments(table, &rest)?;
        if values.len() != table.fields.len() || row.len() != key.len() + values.len() {
            return refuse("a row names every key and register column once");
        }
        table.components(&key)?;
        Ok((key, values))
    }
    /// A new incarnation of `row`, which names every key and register
    /// column. `replaces` are the incarnations `INSERT OR REPLACE` removes.
    pub fn insert(
        &self,
        table: &str,
        row: &[(&str, SqlValue)],
        replaces: &[OperationId],
    ) -> Result<Mutation, TypedError> {
        let table = self.by_name(table)?;
        let (key, values) = Self::split(table, row)?;
        Ok(Self::mutation(
            None,
            Action::Insert {
                table: table.id,
                key,
                values,
                replaces: Self::ids(replaces)?,
            },
        ))
    }
    /// Updates exactly the numeric incarnation, including a hidden one on receive.
    pub fn update_id(
        &self,
        rows: &[TypedRow],
        table: &str,
        id: rocky_core::RowIdentity,
        set: &[(&str, SqlValue)],
    ) -> Result<Mutation, TypedError> {
        let mut matching = rows
            .iter()
            .filter(|row| row.table == table && row.identity == Some(id));
        let row = matching
            .next()
            .ok_or_else(|| TypedError("unknown row id".into()))?;
        if matching.next().is_some() {
            return refuse("ambiguous row id; name its partition");
        }
        let plan = self.by_name(table)?;
        let set = Self::assignments(plan, set)?;
        if set.is_empty()
            || set.iter().any(|(id, _)| {
                plan.fields.iter().any(|field| {
                    field.id == *id && ["id", "row_kind"].contains(&field.name.as_str())
                })
            })
        {
            return refuse("update requires mutable columns");
        }
        Ok(Self::mutation(
            Some(row.incarnation),
            Action::Update {
                table: plan.id,
                set,
                identity: id.integer(),
            },
        ))
    }
    /// Deletes exactly the numeric incarnation.
    pub fn delete_id(
        &self,
        rows: &[TypedRow],
        table: &str,
        id: rocky_core::RowIdentity,
    ) -> Result<Mutation, TypedError> {
        let mut matching = rows
            .iter()
            .filter(|row| row.table == table && row.identity == Some(id));
        let row = matching
            .next()
            .ok_or_else(|| TypedError("unknown row id".into()))?;
        if matching.next().is_some() {
            return refuse("ambiguous row id; name its partition");
        }
        Ok(Self::mutation(
            Some(row.incarnation),
            Action::Delete {
                table: self.by_name(table)?.id,
                observed: Vec::new(),
                identity: id.integer(),
            },
        ))
    }
    /// Updates an identity qualified by its partition values.
    pub fn update_partition_id(
        &self,
        rows: &[TypedRow],
        table: &str,
        values: &[SqlValue],
        id: rocky_core::RowIdentity,
        set: &[(&str, SqlValue)],
    ) -> Result<Mutation, TypedError> {
        let rows = self.partition_rows(rows, table, values)?;
        self.update_id(&rows, table, id, set)
    }
    pub fn delete_partition_id(
        &self,
        rows: &[TypedRow],
        table: &str,
        values: &[SqlValue],
        id: rocky_core::RowIdentity,
    ) -> Result<Mutation, TypedError> {
        let rows = self.partition_rows(rows, table, values)?;
        self.delete_id(&rows, table, id)
    }
    fn partition_rows(
        &self,
        rows: &[TypedRow],
        table: &str,
        values: &[SqlValue],
    ) -> Result<Vec<TypedRow>, TypedError> {
        let plan = self.by_name(table)?;
        let allocation = plan.allocation(values)?;
        rows.iter()
            .filter(|row| row.table == table)
            .filter_map(|row| match plan.allocation_at(&row.key) {
                Ok(scope) if scope == allocation => Some(Ok(row.clone())),
                Ok(_) => None,
                Err(error) => Some(Err(error)),
            })
            .collect()
    }
    /// Historical operation targets are refused; use `update_id`.
    pub fn update(
        &self,
        _table: &str,
        _incarnation: OperationId,
        _set: &[(&str, SqlValue)],
    ) -> Result<Mutation, TypedError> {
        refuse("use update_id for a numeric incarnation")
    }
    /// Historical operation targets are refused; use `delete_id`.
    pub fn delete(
        &self,
        _table: &str,
        _incarnation: OperationId,
        _observed: &[OperationId],
    ) -> Result<Mutation, TypedError> {
        refuse("use delete_id for a numeric incarnation")
    }
    /// The incarnations among `rows` at `key`, compared as the key columns'
    /// affinities and collations compare.
    pub fn locate(
        &self,
        rows: &[TypedRow],
        table: &str,
        key: &[SqlValue],
    ) -> Result<Located, TypedError> {
        let plan = self.by_name(table)?;
        let wanted = plan.components(key)?;
        let mut located = Located::default();
        for row in rows.iter().filter(|row| row.table == table) {
            if plan.components(&row.key)? == wanted {
                located.live.push(row.incarnation);
                if row.visible {
                    located.visible = Some(row.incarnation);
                }
            }
        }
        Ok(located)
    }
    /// `DELETE` by key: the visible incarnation and every other one held.
    /// None when no row is visible.
    pub fn delete_key(
        &self,
        rows: &[TypedRow],
        table: &str,
        key: &[SqlValue],
    ) -> Result<Option<Mutation>, TypedError> {
        let located = self.locate(rows, table, key)?;
        located
            .visible
            .map(|visible| {
                if let Some(id) = rows
                    .iter()
                    .find(|row| row.incarnation == visible)
                    .and_then(|row| row.identity)
                {
                    let wanted = rows
                        .iter()
                        .filter(|row| row.incarnation == visible)
                        .cloned()
                        .collect::<Vec<_>>();
                    let mut mutation = self.delete_id(&wanted, table, id)?;
                    mutation.payload = Action::Delete {
                        table: self.by_name(table)?.id,
                        identity: id.integer(),
                        observed: Self::ids(
                            &located
                                .live
                                .into_iter()
                                .filter(|other| *other != visible)
                                .collect::<Vec<_>>(),
                        )?,
                    }
                    .encode();
                    Ok(mutation)
                } else {
                    refuse("missing numeric incarnation id")
                }
            })
            .transpose()
    }
    /// `INSERT OR REPLACE`: a new incarnation replacing every one held.
    pub fn replace(
        &self,
        rows: &[TypedRow],
        table: &str,
        row: &[(&str, SqlValue)],
    ) -> Result<Mutation, TypedError> {
        let (key, _) = Self::split(self.by_name(table)?, row)?;
        let located = self.locate(rows, table, &key)?;
        self.insert(table, row, &located.live)
    }
    /// `@Upsert`: an insert, or an update of the visible incarnation. The
    /// installed update policy selects the published columns; None when the
    /// policy is `changed` and nothing differs.
    pub fn upsert(
        &self,
        rows: &[TypedRow],
        table: &str,
        row: &[(&str, SqlValue)],
    ) -> Result<Option<Mutation>, TypedError> {
        let plan = self.by_name(table)?;
        let (key, values) = Self::split(plan, row)?;
        let located = self.locate(rows, table, &key)?;
        let Some(visible) = located.visible else {
            return self.insert(table, row, &located.live).map(Some);
        };
        let current = rows
            .iter()
            .find(|row| row.table == table && row.incarnation == visible)
            .ok_or_else(|| TypedError("visible row is missing".into()))?;
        if current.identity != plan.row_identity(&key, &values)? {
            return refuse("upsert cannot change the row id");
        }
        let set: Vec<(&str, SqlValue)> = plan
            .fields
            .iter()
            .zip(values)
            .filter(|(field, (_, value))| {
                !["id", "row_kind"].contains(&field.name.as_str())
                    && (self.0.schema.update_policy == UpdatePolicy::Assigned
                        || !current
                            .values
                            .iter()
                            .any(|(name, held)| *name == field.name && held == value))
            })
            .map(|(field, (_, value))| (field.name.as_str(), value))
            .collect();
        if set.is_empty() {
            return Ok(None);
        }
        match current.identity {
            Some(id) => self.update_id(rows, table, id, &set).map(Some),
            None => refuse("missing numeric incarnation id"),
        }
    }
    fn row(&self, id: &RowId, entry: &Incarnation) -> Vec<SqlValue> {
        let [node, sequence] = id_values(id.2);
        let (tombstoned, state) = match entry {
            Incarnation::Tombstone(identity) => {
                (1, identity.map_or(SqlValue::Null, SqlValue::Integer))
            }
            Incarnation::Live(live) => (0, SqlValue::Blob(live.encode())),
            Incarnation::Moved(moved) => (3, SqlValue::Blob(moved.encode())),
        };
        vec![
            SqlValue::Blob(id.0.to_vec()),
            SqlValue::Blob(id.1.clone()),
            node,
            sequence,
            SqlValue::Integer(tombstoned),
            state,
        ]
    }
}

impl Application for TypedApp {
    type State = State;
    type Error = TypedError;
    type Policy = Access;
    fn validate(&self, envelope: &Envelope) -> Result<(), TypedError> {
        self.action(envelope).map(|_| ())
    }
    fn admit(&self, envelope: &Envelope, policy: &Access) -> Result<(), TypedError> {
        if envelope.scope != policy.scope
            || envelope.caller != policy.caller
            || !policy.nodes.contains(&envelope.id.node)
        {
            return refuse("operation denied");
        }
        self.validate(envelope)
    }
    fn definite_drop(&self, state: &State, envelope: &Envelope) -> bool {
        let Ok(action) = self.action(envelope) else {
            return false;
        };
        let (table, identity) = match action {
            Action::Update {
                table, identity, ..
            }
            | Action::Delete {
                table, identity, ..
            } => (table, identity),
            Action::Insert { .. } | Action::Move { .. } | Action::Follow { .. } => return false,
        };
        match state.entity(envelope.entity) {
            None => true,
            Some(row) if row.0 == table => {
                matches!(state.0.get(row), Some(Incarnation::Tombstone(Some(held))) if *held == identity)
            }
            _ => false,
        }
    }
    /// A function of the applied operation set: incoming order cannot change
    /// the result. The local boundary adds only refusals.
    fn apply_ready(
        &self,
        state: &mut State,
        envelope: &Envelope,
        boundary: AdmissionBoundary,
    ) -> Result<(), TypedError> {
        let local = boundary == AdmissionBoundary::Local;
        let stamp = Stamp::from(envelope);
        match self.action(envelope)? {
            Action::Move {
                table,
                identity,
                key,
                values,
                birth,
            } => {
                let plan = self.by_id(&table)?;
                let source = state
                    .entity(envelope.entity)
                    .cloned()
                    .ok_or_else(|| TypedError("move source missing".into()))?;
                let entry = state
                    .0
                    .get(&source)
                    .cloned()
                    .ok_or_else(|| TypedError("move source missing".into()))?;
                if source.0 != table || plan.incarnation_identity(&entry)? != Some(identity) {
                    return refuse("move source identity");
                }
                let old_key = key_values(&source.1).map_err(failed)?;
                for (position, (old, new)) in old_key.iter().zip(&key).enumerate() {
                    if !plan.partitions.contains(&position) && old != new {
                        return refuse("move changes immutable key");
                    }
                }
                let components = plan.components(&key)?;
                if components == source.1 {
                    return refuse("move destination equals source");
                }
                if local
                    && (state.visible(&source.0, &source.1).map(|(id, _)| id)
                        != Some(envelope.entity)
                        || state.live(&table, &components).next().is_some())
                {
                    return refuse("move source missing or destination occupied");
                }
                let mut pending = Vec::new();
                match &entry {
                    Incarnation::Live(live) => {
                        if live.birth != birth {
                            return refuse("move changes creation proof");
                        }
                        for (field, ((value, original), (_, _, carried))) in
                            plan.fields.iter().zip(live.registers.iter().zip(&values))
                        {
                            if original > carried {
                                pending.push((field.id, value.clone(), *original));
                            }
                        }
                    }
                    Incarnation::Moved(moved) if !local && moved.birth == birth => {
                        pending.clone_from(&moved.pending);
                    }
                    _ => return refuse("move source deleted"),
                }
                state
                    .0
                    .entry((table, components, envelope.id))
                    .or_insert_with(|| {
                        Incarnation::Live(Live {
                            birth,
                            lamport: envelope.lamport,
                            key,
                            registers: values
                                .into_iter()
                                .map(|(_, value, stamp)| (value, stamp))
                                .collect(),
                        })
                    });
                if !matches!(entry, Incarnation::Moved(ref moved) if moved.at > stamp) {
                    state.0.insert(
                        source,
                        Incarnation::Moved(Moved {
                            at: stamp,
                            birth,
                            identity,
                            target: envelope.id,
                            pending,
                        }),
                    );
                }
            }
            Action::Follow {
                table,
                identity,
                source,
                set,
            } => {
                let destination = state
                    .entity(envelope.entity)
                    .cloned()
                    .ok_or_else(|| TypedError("follow destination missing".into()))?;
                if destination.0 != table
                    || self
                        .by_id(&table)?
                        .incarnation_identity(&state.0[&destination])?
                        != Some(identity)
                {
                    return refuse("follow destination identity");
                }
                let Some(source_row) = state.entity(source).cloned() else {
                    return refuse("follow source missing");
                };
                let Some(Incarnation::Moved(moved)) = state.0.get(&source_row) else {
                    return refuse("follow source is not moved");
                };
                if moved.identity != identity || source_row.0 != table {
                    return refuse("follow source identity");
                }
                let plan = self.by_id(&table)?;
                match state.0.get_mut(&destination) {
                    Some(Incarnation::Live(live)) => {
                        for (field, value, original) in &set {
                            let register = &mut live.registers[plan
                                .field(field)
                                .ok_or_else(|| TypedError("follow field missing".into()))?];
                            if *original > register.1 {
                                *register = (value.clone(), *original);
                            }
                        }
                    }
                    Some(Incarnation::Moved(moved)) => {
                        for (field, value, original) in &set {
                            remember(&mut moved.pending, *field, value.clone(), *original);
                        }
                    }
                    _ => {}
                }
                if let Some(Incarnation::Moved(moved)) = state.0.get_mut(&source_row) {
                    moved.pending.retain(|(field, _, original)| {
                        !set.iter().any(|(followed, _, stamp)| {
                            field == followed && original.node == stamp.node && original <= stamp
                        })
                    });
                }
            }
            Action::Insert {
                table,
                key,
                values,
                replaces,
            } => {
                let plan = self.by_id(&table)?;
                let components = plan.components(&key)?;
                if let Some(identity) = plan.row_identity(&key, &values)? {
                    if self.1.get(&(envelope.id.node, plan.name.clone()))
                        != Some(&identity.author())
                    {
                        return refuse("forged row author number");
                    }
                    let allocation = plan.allocation_at(&key)?;
                    let highest = state
                        .1
                        .get(&(allocation, identity.author()))
                        .copied()
                        .unwrap_or(0);
                    if identity.sequence() <= highest
                        || (local
                            && identity.sequence()
                                != highest
                                    .checked_add(1)
                                    .ok_or_else(|| TypedError("row sequence exhausted".into()))?)
                    {
                        return refuse("reused or skipped row identity sequence");
                    }
                    state
                        .1
                        .insert((allocation, identity.author()), identity.sequence());
                }
                if local
                    && state
                        .live(&table, &components)
                        .any(|(id, _)| !replaces.contains(&id))
                {
                    return refuse("primary key already exists");
                }
                // A tombstone that arrived first learns the immutable ID.
                if let Some(Incarnation::Tombstone(identity)) =
                    state.0.get_mut(&(table, components.clone(), envelope.id))
                {
                    *identity = plan
                        .row_identity(&key, &values)?
                        .map(rocky_core::RowIdentity::integer);
                }
                state
                    .0
                    .entry((table, components.clone(), envelope.id))
                    .or_insert_with(|| {
                        Incarnation::Live(Live {
                            birth: stamp,
                            lamport: envelope.lamport,
                            key,
                            registers: values
                                .into_iter()
                                .map(|(_, value)| (value, stamp))
                                .collect(),
                        })
                    });
                for replaced in replaces {
                    let identity = state
                        .0
                        .get(&(table, components.clone(), replaced))
                        .map(|entry| plan.incarnation_identity(entry))
                        .transpose()?
                        .flatten();
                    state.0.insert(
                        (table, components.clone(), replaced),
                        Incarnation::Tombstone(identity),
                    );
                }
            }
            Action::Update {
                table,
                set,
                identity,
            } => {
                let Some(row) = state.entity(envelope.entity).cloned() else {
                    return refuse("unknown incarnation");
                };
                if row.0 != table {
                    return refuse("incarnation belongs to another table");
                }
                if self.by_id(&table)?.incarnation_identity(
                    state
                        .0
                        .get(&row)
                        .ok_or_else(|| TypedError("missing incarnation".into()))?,
                )? != Some(identity)
                {
                    return refuse("numeric id differs from the target incarnation");
                }
                let visible = state.visible(&row.0, &row.1).map(|(id, _)| id);
                let plan = self.by_id(&table)?;
                match state.0.get_mut(&row) {
                    Some(Incarnation::Live(live)) => {
                        if local && visible != Some(envelope.entity) {
                            return refuse("row is not visible");
                        }
                        for (id, value) in set {
                            let register = plan
                                .field(&id)
                                .and_then(|field| live.registers.get_mut(field))
                                .ok_or_else(|| TypedError("unknown field".into()))?;
                            if stamp > register.1 {
                                *register = (value, stamp);
                            }
                        }
                    }
                    Some(Incarnation::Moved(moved)) if !local => {
                        for (field, value) in set {
                            remember(&mut moved.pending, field, value, stamp);
                        }
                    }
                    _ if local => return refuse("row no longer exists"),
                    // A late update cannot resurrect a deleted incarnation.
                    _ => {}
                }
            }
            Action::Delete {
                table,
                observed,
                identity,
            } => {
                let Some(row) = state.entity(envelope.entity).cloned() else {
                    return refuse("unknown incarnation");
                };
                if row.0 != table {
                    return refuse("incarnation belongs to another table");
                }
                if self.by_id(&table)?.incarnation_identity(
                    state
                        .0
                        .get(&row)
                        .ok_or_else(|| TypedError("missing incarnation".into()))?,
                )? != Some(identity)
                {
                    return refuse("numeric id differs from the target incarnation");
                }
                if local {
                    if state.visible(&row.0, &row.1).map(|(id, _)| id) != Some(envelope.entity) {
                        return refuse("row no longer exists");
                    }
                    if state
                        .live(&row.0, &row.1)
                        .any(|(id, _)| id != envelope.entity && !observed.contains(&id))
                    {
                        return refuse("another incarnation of this key appeared");
                    }
                }
                for id in observed.into_iter().chain([envelope.entity]) {
                    let identity = state
                        .0
                        .get(&(row.0, row.1.clone(), id))
                        .map(|entry| self.by_id(&table)?.incarnation_identity(entry))
                        .transpose()?
                        .flatten();
                    state
                        .0
                        .insert((row.0, row.1.clone(), id), Incarnation::Tombstone(identity));
                }
            }
        }
        Ok(())
    }
}
impl Table {
    fn incarnation_identity(&self, entry: &Incarnation) -> Result<Option<i64>, TypedError> {
        match entry {
            Incarnation::Tombstone(identity) => Ok(*identity),
            Incarnation::Moved(moved) => Ok(Some(moved.identity)),
            Incarnation::Live(live) => {
                let values = self
                    .fields
                    .iter()
                    .zip(&live.registers)
                    .map(|(field, (value, _))| (field.id, value.clone()))
                    .collect();
                Ok(self
                    .row_identity(&live.key, &values)?
                    .map(rocky_core::RowIdentity::integer))
            }
        }
    }
    fn row_identity(
        &self,
        key: &[SqlValue],
        values: &Assignments,
    ) -> Result<Option<rocky_core::RowIdentity>, TypedError> {
        let value = self
            .key_names
            .iter()
            .position(|name| name == "id")
            .and_then(|index| key.get(index))
            .or_else(|| {
                self.fields
                    .iter()
                    .position(|field| field.name == "id")
                    .and_then(|index| values.get(index).map(|(_, value)| value))
            });
        let Some(SqlValue::Integer(id)) = value else {
            return refuse("missing row identity");
        };
        Ok(Some(
            rocky_core::RowIdentity::from_integer(*id).map_err(failed)?,
        ))
    }
    /// The key and register values of a captured row image.
    fn image(&self, row: &[SqlValue]) -> Result<(Vec<SqlValue>, Assignments), TypedError> {
        if row.len() != self.width {
            return refuse(format!("{} changed its columns", self.name));
        }
        let key = self
            .key_positions
            .iter()
            .map(|position| row[*position].clone())
            .collect();
        let values = self
            .fields
            .iter()
            .map(|field| (field.id, row[field.position].clone()))
            .collect();
        Ok((key, values))
    }
}
/// A captured row becomes at most one action on its key. A key that was
/// deleted and inserted again, or whose spelling changed, is a new
/// incarnation that replaces the ones held; see `docs/runtime/sql-sessions.md`.
impl SqlWrites<TypedApp> for TypedApp {
    fn identity_allocation(
        &self,
        table: &str,
        node: NodeId,
    ) -> Result<Option<(u32, ProjectionStatement)>, TypedError> {
        self.identity_allocation_partition(table, node, &[])
    }
    fn identity_allocation_partition(
        &self,
        table: &str,
        node: NodeId,
        values: &[SqlValue],
    ) -> Result<Option<(u32, ProjectionStatement)>, TypedError> {
        let table = self.by_name(table)?;
        let allocation = table.allocation(values)?;
        let author = *self
            .1
            .get(&(node, table.name.clone()))
            .ok_or_else(|| TypedError("register a row author for this table and node".into()))?;
        Ok(Some((
            author,
            statement(
                "SELECT COALESCE(MAX(sequence),0) FROM rocky_identity_marks WHERE tbl=? AND author=?",
                vec![
                    SqlValue::Blob(allocation.to_vec()),
                    SqlValue::Integer(i64::from(author)),
                ],
            ),
        )))
    }
    fn located(&self, row: &CapturedRow) -> Result<Vec<ProjectionStatement>, TypedError> {
        let table = self.by_name(&row.table)?;
        let Some(image) = row.before.as_ref().or(row.after.as_ref()) else {
            return Ok(Vec::new());
        };
        let components = table.components(&table.image(image)?.0)?;
        Ok(vec![statement(
            LIVE_AT_KEY,
            vec![
                SqlValue::Blob(table.id.to_vec()),
                SqlValue::Blob(components),
            ],
        )])
    }
    fn intent(&self, state: &State, row: &CapturedRow) -> Result<Option<Intent>, TypedError> {
        let table = self.by_name(&row.table)?;
        let before = row
            .before
            .as_deref()
            .map(|row| table.image(row))
            .transpose()?;
        let after = row
            .after
            .as_deref()
            .map(|row| table.image(row))
            .transpose()?;
        let Some((key, _)) = before.as_ref().or(after.as_ref()) else {
            return Ok(None);
        };
        let components = table.components(key)?;
        let live: Vec<_> = state
            .live(&table.id, &components)
            .map(|(id, _)| id)
            .collect();
        let visible = state.visible(&table.id, &components).map(|(id, _)| id);
        let shown = || visible.ok_or_else(|| TypedError("row is not visible".into()));
        let identity = before
            .as_ref()
            .map(|(key, values)| table.row_identity(key, values))
            .transpose()?
            .flatten()
            .map(rocky_core::RowIdentity::integer);
        let (entity, action) = match (before, after) {
            (None, None) => return Ok(None),
            (Some(_), None) => {
                let visible = shown()?;
                let observed: Vec<_> = live.into_iter().filter(|id| *id != visible).collect();
                let observed = Self::ids(&observed)?;
                (
                    Some(visible),
                    Action::Delete {
                        identity: identity
                            .ok_or_else(|| TypedError("missing numeric incarnation id".into()))?,
                        table: table.id,
                        observed,
                    },
                )
            }
            (Some((spelled, held)), Some((key, values))) if !row.reincarnated && spelled == key => {
                let assigned = self.0.schema.update_policy == UpdatePolicy::Assigned;
                let set: Assignments = table
                    .fields
                    .iter()
                    .zip(held.into_iter().zip(values))
                    .filter(|(field, (held, value))| {
                        held.1 != value.1
                            || (assigned && row.assigned.get(field.position) == Some(&true))
                    })
                    .map(|(_, (_, value))| value)
                    .collect();
                if set.is_empty() {
                    return Ok(None);
                }
                (
                    Some(shown()?),
                    Action::Update {
                        identity: identity
                            .ok_or_else(|| TypedError("missing numeric incarnation id".into()))?,
                        table: table.id,
                        set,
                    },
                )
            }
            (_, Some((key, values))) => (
                None,
                Action::Insert {
                    table: table.id,
                    key,
                    values,
                    replaces: Self::ids(&live)?,
                },
            ),
        };
        Ok(Some(Intent {
            entity,
            payload: action.encode(),
        }))
    }
}
impl ProjectionSchema for TypedApp {
    fn installed_schema(&self) -> InstalledSchema {
        self.0.installed.clone()
    }
}
impl StoredApplication for TypedApp {
    type Row = TypedRow;
    fn descriptor(&self) -> ApplicationDescriptor {
        ApplicationDescriptor {
            definition: DEFINITION,
            operations: Versions::V1,
            projection: 1,
        }
    }
    fn manifest(&self) -> Option<ApplicationManifest> {
        Some(self.0.manifest.clone())
    }
    fn logical_schema(&self) -> Option<&LogicalSchema> {
        Some(&self.0.schema)
    }
    fn sql_writes(&self) -> Option<&dyn SqlWrites<Self>> {
        Some(self)
    }
    fn following_queries(&self, node: NodeId) -> Vec<ProjectionStatement> {
        vec![statement(
            "WITH RECURSIVE paths(node,sequence) AS (SELECT source_node,source_sequence FROM rocky_follow_fields WHERE original_node=? UNION SELECT i.target_node,i.target_sequence FROM rocky_incarnations i JOIN paths p ON i.node=p.node AND i.sequence=p.sequence WHERE i.target_node IS NOT NULL) SELECT i.tbl,i.row_key,i.node,i.sequence,i.tombstoned,i.state FROM rocky_incarnations i JOIN paths p ON i.node=p.node AND i.sequence=p.sequence",
            vec![SqlValue::Blob(node.0.to_vec())],
        )]
    }
    fn following_intents(
        &self,
        rows: Vec<Vec<SqlValue>>,
        node: NodeId,
    ) -> Result<Vec<rocky_core::AuthoredFollow>, TypedError> {
        let state = self.window(rows)?;
        let mut follows = Vec::new();
        for ((table, _, source), entry) in &state.0 {
            let Incarnation::Moved(moved) = entry else {
                continue;
            };
            let mut target = moved.target;
            let mut visited = BTreeSet::from([*source]);
            let destination = loop {
                if !visited.insert(target) {
                    return refuse("move link cycle");
                }
                let Some(row) = state.entity(target) else {
                    break None;
                };
                match &state.0[row] {
                    Incarnation::Live(_) => break Some(target),
                    Incarnation::Moved(next) => target = next.target,
                    _ => break None,
                }
            };
            let Some(destination) = destination else {
                continue;
            };
            let mut grouped: BTreeMap<OperationId, Vec<_>> = BTreeMap::new();
            for (field, value, stamp) in &moved.pending {
                if stamp.node == node {
                    grouped
                        .entry(OperationId {
                            node,
                            sequence: stamp.sequence,
                        })
                        .or_default()
                        .push((*field, value.clone(), *stamp));
                }
            }
            for (original, set) in grouped {
                let mut handle = Vec::from(&b"rocky.typed.follow.v1"[..]);
                for operation in [*source, original, destination] {
                    handle.extend(operation.node.0);
                    handle.extend(operation.sequence.get().to_le_bytes());
                }
                follows.push(rocky_core::AuthoredFollow {
                    handle: Sha256::digest(&handle).into(),
                    original,
                    intent: Intent {
                        entity: Some(destination),
                        payload: Action::Follow {
                            table: *table,
                            identity: moved.identity,
                            source: *source,
                            set,
                        }
                        .encode(),
                    },
                });
            }
        }
        Ok(follows)
    }
    fn projection_rows(&self, state: &State) -> Vec<Vec<SqlValue>> {
        state
            .0
            .iter()
            .map(|(id, entry)| self.row(id, entry))
            .chain(state.1.iter().map(|((table, author), sequence)| {
                vec![
                    SqlValue::Blob(table.to_vec()),
                    SqlValue::Integer(i64::from(*author)),
                    SqlValue::Integer(i64::from(*sequence)),
                ]
            }))
            .collect()
    }
    /// Live rows at the operation's key, and each incarnation it names.
    fn touched(&self, envelope: &Envelope) -> Vec<ProjectionStatement> {
        let entity = id_values(envelope.entity);
        let with = |other: OperationId| {
            let mut values = entity.to_vec();
            values.extend(id_values(other));
            statement(ONE_WITH, values)
        };
        match Action::decode(&envelope.payload) {
            Ok(Action::Move { table, key, .. }) => {
                let Ok(components) = self.by_id(&table).and_then(|plan| plan.components(&key))
                else {
                    return Vec::new();
                };
                let mut at = vec![SqlValue::Blob(table.to_vec()), SqlValue::Blob(components)];
                let mut queries = vec![
                    statement(ENTITY, entity.to_vec()),
                    statement(LIVE_WITH, entity.to_vec()),
                    statement(LIVE_AT_KEY, at.clone()),
                ];
                at.extend(id_values(envelope.id));
                queries.push(statement(ONE_AT_KEY, at));
                queries
            }
            Ok(Action::Follow { source, .. }) => vec![
                statement(ENTITY, entity.to_vec()),
                statement(LIVE_WITH, entity.to_vec()),
                statement(ENTITY, id_values(source).to_vec()),
            ],
            Ok(Action::Insert {
                table,
                key,
                replaces,
                ..
            }) => {
                let Ok(components) = self.by_id(&table).and_then(|plan| plan.components(&key))
                else {
                    return Vec::new();
                };
                let at = [SqlValue::Blob(table.to_vec()), SqlValue::Blob(components)];
                let mut statements = vec![statement(LIVE_AT_KEY, at.to_vec())];
                if let Ok(action) = self.action(envelope)
                    && let Action::Insert { key, values, .. } = action
                    && let Ok(Some(identity)) = self
                        .by_id(&table)
                        .and_then(|plan| plan.row_identity(&key, &values))
                    && let Ok(allocation) =
                        self.by_id(&table).and_then(|plan| plan.allocation_at(&key))
                {
                    statements.push(self.mark_query(allocation, identity.author()));
                }
                for id in replaces.into_iter().chain([envelope.id]) {
                    let mut values = at.to_vec();
                    values.extend(id_values(id));
                    statements.push(statement(ONE_AT_KEY, values));
                }
                statements
            }
            Ok(Action::Update { .. }) => vec![
                statement(LIVE_WITH, entity.to_vec()),
                statement(ENTITY, entity.to_vec()),
            ],
            Ok(Action::Delete { observed, .. }) => [
                statement(LIVE_WITH, entity.to_vec()),
                statement(ENTITY, entity.to_vec()),
            ]
            .into_iter()
            .chain(observed.into_iter().map(with))
            .collect(),
            Err(_) => Vec::new(),
        }
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<State, TypedError> {
        let mut state = State::default();
        for values in rows {
            let mark = match values.as_slice() {
                [
                    SqlValue::Blob(table),
                    SqlValue::Integer(author),
                    SqlValue::Integer(sequence),
                ] => Some((table, *author, *sequence)),
                [
                    SqlValue::Blob(table),
                    SqlValue::Null,
                    SqlValue::Null,
                    SqlValue::Integer(sequence),
                    SqlValue::Integer(author),
                    SqlValue::Null,
                ] => Some((table, *author, *sequence)),
                _ => None,
            };
            if let Some((table, author, sequence)) = mark {
                let table = table
                    .as_slice()
                    .try_into()
                    .map_err(|_| TypedError("invalid mark table".into()))?;
                let author = u32::try_from(author).map_err(failed)?;
                let sequence = u32::try_from(sequence).map_err(failed)?;
                rocky_core::RowIdentity::new(author, sequence).map_err(failed)?;
                state.1.insert((table, author), sequence);
                continue;
            }
            let [
                SqlValue::Blob(table),
                SqlValue::Blob(key),
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
                SqlValue::Integer(tombstoned),
                stored,
            ] = values.as_slice()
            else {
                return refuse("invalid incarnation row");
            };
            let table: Commitment = table
                .as_slice()
                .try_into()
                .map_err(|_| TypedError("invalid table identity".into()))?;
            let plan = self.by_id(&table)?;
            let id = OperationId {
                node: NodeId(
                    node.as_slice()
                        .try_into()
                        .map_err(|_| TypedError("invalid node".into()))?,
                ),
                sequence: AuthorSequence::new(*sequence as u64).map_err(failed)?,
            };
            let entry = match (tombstoned, stored) {
                (1, SqlValue::Null) => {
                    key_values(key).map_err(failed)?;
                    Incarnation::Tombstone(None)
                }
                (1, SqlValue::Integer(identity)) => {
                    rocky_core::RowIdentity::from_integer(*identity).map_err(failed)?;
                    Incarnation::Tombstone(Some(*identity))
                }
                (3, SqlValue::Blob(bytes)) => Incarnation::Moved(Moved::decode(bytes)?),
                (0, SqlValue::Blob(bytes)) => {
                    let live = Live::decode(bytes)?;
                    if plan.components(&live.key)? != *key
                        || live.registers.len() != plan.fields.len()
                    {
                        return refuse("incarnation differs from its key or table");
                    }
                    for (field, (value, _)) in plan.fields.iter().zip(&live.registers) {
                        Table::stored(field, value)?;
                    }
                    Incarnation::Live(live)
                }
                _ => return refuse("invalid incarnation tombstone"),
            };
            // Overlapping reads of one window return a row more than once.
            if let Some(known) = state.0.insert((table, key.clone(), id), entry.clone())
                && known != entry
            {
                return refuse("conflicting incarnation rows");
            }
        }
        Ok(state)
    }
    /// Stores the window and rewrites the typed row of each key in it.
    fn project(&self, state: &State, _operation: &Envelope) -> Vec<ProjectionStatement> {
        let mut statements: Vec<_> = state
            .0
            .iter()
            .map(|(id, entry)| statement(STORE, self.row(id, entry)))
            .collect();
        for ((_, _, source), entry) in &state.0 {
            if let Incarnation::Moved(moved) = entry {
                statements.push(statement("UPDATE rocky_incarnations SET target_node=?,target_sequence=? WHERE node=? AND sequence=?", vec![SqlValue::Blob(moved.target.node.0.to_vec()), SqlValue::Integer(moved.target.sequence.get() as i64), SqlValue::Blob(source.node.0.to_vec()), SqlValue::Integer(source.sequence.get() as i64)]));
                statements.push(statement(
                    "DELETE FROM rocky_follow_fields WHERE source_node=? AND source_sequence=?",
                    id_values(*source).to_vec(),
                ));
                for (field, _, stamp) in &moved.pending {
                    statements.push(statement(
                        "INSERT INTO rocky_follow_fields VALUES(?,?,?,?,?)",
                        vec![
                            SqlValue::Blob(source.node.0.to_vec()),
                            SqlValue::Integer(source.sequence.get() as i64),
                            SqlValue::Blob(stamp.node.0.to_vec()),
                            SqlValue::Integer(stamp.sequence.get() as i64),
                            SqlValue::Blob(field.to_vec()),
                        ],
                    ));
                }
            }
        }
        statements.extend(state.1.iter().map(|((table, author), sequence)| statement("INSERT INTO rocky_identity_marks VALUES(?,?,?) ON CONFLICT(tbl,author) DO UPDATE SET sequence=MAX(sequence,excluded.sequence)", vec![SqlValue::Blob(table.to_vec()), SqlValue::Integer(i64::from(*author)), SqlValue::Integer(i64::from(*sequence))])));
        let keys: BTreeSet<_> = state.0.keys().map(|(table, key, _)| (table, key)).collect();
        for (table, key) in keys {
            let Ok(plan) = self.by_id(table) else {
                continue;
            };
            statements.push(match state.visible(table, key) {
                Some((_, live)) => ProjectionStatement {
                    sql: plan.upsert.clone().into(),
                    values: live
                        .key
                        .iter()
                        .chain(live.registers.iter().map(|(value, _)| value))
                        .cloned()
                        .collect(),
                },
                None => ProjectionStatement {
                    sql: plan.delete.clone().into(),
                    values: key_values(key).unwrap_or_default(),
                },
            });
        }
        statements
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<TypedRow>, TypedError> {
        let state = self.window(rows)?;
        let mut decoded = Vec::new();
        for ((table, key, id), entry) in &state.0 {
            let Incarnation::Live(live) = entry else {
                continue;
            };
            let plan = self.by_id(table)?;
            decoded.push(TypedRow {
                table: plan.name.clone(),
                incarnation: *id,
                identity: plan
                    .incarnation_identity(entry)?
                    .map(rocky_core::RowIdentity::from_integer)
                    .transpose()
                    .map_err(failed)?,
                visible: state.visible(table, key).map(|(id, _)| id) == Some(*id),
                key: live.key.clone(),
                birth: live.birth,
                stamps: plan
                    .fields
                    .iter()
                    .zip(&live.registers)
                    .map(|(field, (_, stamp))| (field.name.clone(), *stamp))
                    .collect(),
                values: plan
                    .fields
                    .iter()
                    .zip(&live.registers)
                    .map(|(field, (value, _))| (field.name.clone(), value.clone()))
                    .collect(),
            });
        }
        Ok(decoded)
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Access,
    ) -> Result<(), TypedError> {
        if scope == policy.scope && caller == policy.caller {
            Ok(())
        } else {
            refuse("read denied")
        }
    }
    fn read_views(
        &self,
        context: AdmissionContext,
        policy: &Access,
    ) -> Result<Vec<ReadView>, TypedError> {
        self.admit_read(context.scope, context.caller, policy)?;
        Ok(self.0.views.clone())
    }
}

#[derive(Clone, PartialEq)]
struct Moved {
    at: Stamp,
    birth: Stamp,
    identity: i64,
    target: OperationId,
    pending: Vec<(Commitment, SqlValue, Stamp)>,
}
fn put_stamped(bytes: &mut Vec<u8>, fields: &[(Commitment, SqlValue, Stamp)]) {
    bytes.extend((fields.len() as u16).to_le_bytes());
    for (field, value, stamp) in fields {
        bytes.extend(field);
        value.encode(bytes);
        stamp_bytes(bytes, *stamp);
    }
}
impl Reader<'_> {
    fn stamp(&mut self) -> Result<Stamp, TypedError> {
        Ok(Stamp {
            lamport: LamportTime::new(u64::from_le_bytes(self.array()?)).map_err(failed)?,
            node: NodeId(self.array()?),
            sequence: AuthorSequence::new(u64::from_le_bytes(self.array()?)).map_err(failed)?,
        })
    }
    fn operation(&mut self) -> Result<OperationId, TypedError> {
        Ok(OperationId {
            node: NodeId(self.array()?),
            sequence: AuthorSequence::new(u64::from_le_bytes(self.array()?)).map_err(failed)?,
        })
    }
    fn stamped(&mut self) -> Result<Vec<(Commitment, SqlValue, Stamp)>, TypedError> {
        let count = u16::from_le_bytes(self.array()?);
        (0..count)
            .map(|_| Ok((self.array()?, self.value()?, self.stamp()?)))
            .collect()
    }
}
impl Moved {
    fn encode(&self) -> Vec<u8> {
        let mut bytes = vec![1];
        stamp_bytes(&mut bytes, self.at);
        stamp_bytes(&mut bytes, self.birth);
        bytes.extend(self.identity.to_le_bytes());
        bytes.extend(self.target.node.0);
        bytes.extend(self.target.sequence.get().to_le_bytes());
        put_stamped(&mut bytes, &self.pending);
        bytes
    }
    fn decode(bytes: &[u8]) -> Result<Self, TypedError> {
        let mut reader = Reader(bytes);
        if reader.array::<1>()? != [1] {
            return refuse("unsupported move state");
        }
        let at = reader.stamp()?;
        let birth = reader.stamp()?;
        let identity = i64::from_le_bytes(reader.array()?);
        rocky_core::RowIdentity::from_integer(identity).map_err(failed)?;
        let target = reader.operation()?;
        let pending = reader.stamped()?;
        if !reader.0.is_empty() {
            return refuse("trailing move state");
        }
        Ok(Self {
            at,
            birth,
            identity,
            target,
            pending,
        })
    }
}
impl TypedApp {
    /// Moves a held incarnation without changing its identity or field ordering.
    pub fn move_row(
        &self,
        rows: &[TypedRow],
        table: &str,
        id: rocky_core::RowIdentity,
        from: &[SqlValue],
        to: &[SqlValue],
    ) -> Result<Mutation, TypedError> {
        let plan = self.by_name(table)?;
        if plan.partitions.is_empty()
            || plan
                .partitions
                .iter()
                .any(|position| plan.key_names[*position] == "id")
        {
            return refuse("move requires a table partitioned independently of its id");
        }
        if from.len() != plan.partitions.len() || to.len() != from.len() || from == to {
            return refuse("move partition tuple");
        }
        let columns: Vec<_> = plan
            .partitions
            .iter()
            .map(|position| plan.key[*position])
            .collect();
        let source = key_components(&columns, from).map_err(failed)?;
        let row = rows
            .iter()
            .find(|row| {
                row.table == table
                    && row.identity == Some(id)
                    && row.visible
                    && key_components(
                        &columns,
                        &plan
                            .partitions
                            .iter()
                            .map(|position| row.key[*position].clone())
                            .collect::<Vec<_>>(),
                    )
                    .ok()
                        == Some(source.clone())
            })
            .ok_or_else(|| TypedError("move source is not visible".into()))?;
        let mut key = row.key.clone();
        for (position, value) in plan.partitions.iter().zip(to) {
            key[*position] = value.clone();
        }
        plan.components(&key)?;
        let values = plan
            .fields
            .iter()
            .map(|field| {
                let value = row
                    .values
                    .iter()
                    .find(|(name, _)| *name == field.name)
                    .map(|(_, value)| value.clone())
                    .ok_or_else(|| TypedError("move field missing".into()))?;
                let stamp = row
                    .stamps
                    .iter()
                    .find(|(name, _)| *name == field.name)
                    .map(|(_, stamp)| *stamp)
                    .ok_or_else(|| TypedError("move stamp missing".into()))?;
                Ok((field.id, value, stamp))
            })
            .collect::<Result<Vec<_>, TypedError>>()?;
        Ok(Self::mutation(
            Some(row.incarnation),
            Action::Move {
                table: plan.id,
                identity: id.integer(),
                key,
                values,
                birth: row.birth,
            },
        ))
    }
}

fn remember(
    pending: &mut Vec<(Commitment, SqlValue, Stamp)>,
    field: Commitment,
    value: SqlValue,
    stamp: Stamp,
) {
    if let Some((_, old, original)) = pending.iter_mut().find(|(id, _, _)| *id == field) {
        if stamp > *original {
            *old = value;
            *original = stamp;
        }
    } else {
        pending.push((field, value, stamp));
        pending.sort_by_key(|(field, _, _)| *field);
    }
}
