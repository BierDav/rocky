//! Probe-only application with a generated schema. It exists to measure wide
//! schemas against today's limits and is not part of the supported API.
use rocky_core::*;
use std::collections::BTreeMap;

pub const MAX_TABLES: u16 = 2000;

/// Generated SQL, leaked once per probe run to satisfy the `'static` contract.
pub struct Layout {
    schema: Vec<&'static str>,
    projection: &'static str,
    names: Vec<&'static str>,
    upsert: Vec<&'static str>,
    /// One stored row by key, in projection shape.
    select: Vec<&'static str>,
    /// The readable rows of each table: live ones, without reducer columns.
    views: Vec<&'static str>,
}
#[derive(Clone, Copy)]
pub struct WideSchema(&'static Layout, u16);
#[derive(Clone)]
pub struct Row {
    pub table: u16,
    pub a: i64,
    pub b: i64,
    pub c: String,
    stamp: Stamp,
    /// Deleted rows stay stored as tombstones so later edits still converge.
    deleted: bool,
}
#[derive(Clone, Default)]
pub struct Rows(BTreeMap<OperationId, Row>);
#[derive(Clone)]
pub struct Policy {
    pub scope: Scope,
    pub caller: PrincipalId,
}
#[derive(Debug)]
pub struct Invalid;
impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid wide-schema operation or authorization")
    }
}
impl std::error::Error for Invalid {}

fn leak(text: String) -> &'static str {
    Box::leak(text.into_boxed_str())
}
impl WideSchema {
    /// Tables `t0..`, each with two indexed columns.
    pub fn new(tables: u16) -> Self {
        let mut layout = Layout {
            schema: Vec::new(),
            projection: "",
            names: Vec::new(),
            upsert: Vec::new(),
            select: Vec::new(),
            views: Vec::new(),
        };
        let mut projection = Vec::new();
        for table in 0..tables.clamp(1, MAX_TABLES) {
            let name = leak(format!("t{table}"));
            layout.names.push(name);
            layout.schema.push(leak(format!("CREATE TABLE {name}(node BLOB,sequence INTEGER,a INTEGER,b INTEGER,c TEXT,lamport INTEGER,stamp_node BLOB,stamp_sequence INTEGER,deleted INTEGER NOT NULL,PRIMARY KEY(node,sequence))")));
            for column in ["a", "b"] {
                layout.schema.push(leak(format!(
                    "CREATE INDEX {name}_{column} ON {name}({column})"
                )));
            }
            layout.upsert.push(leak(format!("INSERT INTO {name} VALUES(?,?,?,?,?,?,?,?,?) ON CONFLICT(node,sequence) DO UPDATE SET a=excluded.a,b=excluded.b,c=excluded.c,lamport=excluded.lamport,stamp_node=excluded.stamp_node,stamp_sequence=excluded.stamp_sequence,deleted=excluded.deleted")));
            let stored = format!(
                "SELECT {table},node,sequence,a,b,c,lamport,stamp_node,stamp_sequence,deleted FROM {name}"
            );
            layout
                .select
                .push(leak(format!("{stored} WHERE node=? AND sequence=?")));
            layout.views.push(leak(format!(
                "SELECT node,sequence,a,b,c FROM {name} WHERE deleted=0"
            )));
            projection.push(stored);
        }
        // The pinned engine overflows its stack on a flat compound of roughly
        // 1,000 terms, so the terms are grouped into subqueries.
        let groups: Vec<_> = projection
            .chunks(32)
            .map(|group| format!("SELECT * FROM ({})", group.join(" UNION ALL ")))
            .collect();
        layout.projection = leak(format!("{} ORDER BY 1,2,3", groups.join(" UNION ALL ")));
        Self(Box::leak(Box::new(layout)), 1)
    }
    pub fn with_domains(self, domains: u16) -> Self {
        Self(self.0, domains)
    }
    pub fn tables(&self) -> u16 {
        self.0.names.len() as u16
    }
    pub fn app_id() -> AppId {
        ApplicationManifest::app_id([0x57; 32], "wide-probe")
    }
    pub fn create(table: u16, a: i64, b: i64, c: &str) -> Vec<u8> {
        let mut payload = vec![0];
        payload.extend(table.to_le_bytes());
        payload.extend(a.to_le_bytes());
        payload.extend(b.to_le_bytes());
        payload.extend(c.as_bytes());
        payload
    }
    /// Edits and deletions name their row's table: a transition must be
    /// able to say which stored row it touches before reading any.
    pub fn set_a(table: u16, a: i64) -> Vec<u8> {
        let mut payload = vec![1];
        payload.extend(table.to_le_bytes());
        payload.extend(a.to_le_bytes());
        payload
    }
    pub fn delete(table: u16) -> Vec<u8> {
        let mut payload = vec![2];
        payload.extend(table.to_le_bytes());
        payload
    }
}
enum Action {
    Create(u16, i64, i64, String),
    SetA(u16, i64),
    Delete(u16),
}
impl Action {
    fn table(&self) -> u16 {
        match self {
            Self::Create(table, ..) | Self::SetA(table, _) | Self::Delete(table) => *table,
        }
    }
}
fn number(bytes: &[u8]) -> Result<i64, Invalid> {
    Ok(i64::from_le_bytes(bytes.try_into().map_err(|_| Invalid)?))
}
fn decode(payload: &[u8]) -> Result<Action, Invalid> {
    match payload {
        [0, rest @ ..] if rest.len() >= 18 && rest.len() <= 18 + 256 => Ok(Action::Create(
            u16::from_le_bytes([rest[0], rest[1]]),
            number(&rest[2..10])?,
            number(&rest[10..18])?,
            String::from_utf8(rest[18..].to_vec()).map_err(|_| Invalid)?,
        )),
        [1, low, high, rest @ ..] => Ok(Action::SetA(
            u16::from_le_bytes([*low, *high]),
            number(rest)?,
        )),
        [2, low, high] => Ok(Action::Delete(u16::from_le_bytes([*low, *high]))),
        _ => Err(Invalid),
    }
}
impl Application for WideSchema {
    type State = Rows;
    type Error = Invalid;
    type Policy = Policy;
    fn validate(&self, op: &Envelope) -> Result<(), Invalid> {
        op.validate_dependencies(op.entity == op.id)
            .map_err(|_| Invalid)?;
        let action = decode(&op.payload)?;
        if action.table() >= self.tables() {
            return Err(Invalid);
        }
        match action {
            Action::Create(..) if op.entity == op.id => Ok(()),
            Action::SetA(..) | Action::Delete(_) if op.entity != op.id => Ok(()),
            _ => Err(Invalid),
        }
    }
    fn admit(&self, op: &Envelope, policy: &Policy) -> Result<(), Invalid> {
        self.admit_read(op.scope, op.caller, policy)
    }
    fn apply_ready(
        &self,
        state: &mut Rows,
        op: &Envelope,
        _: AdmissionBoundary,
    ) -> Result<(), Invalid> {
        Application::validate(self, op)?;
        let stamp = Stamp::from(op);
        match decode(&op.payload)? {
            Action::Create(table, a, b, c) => {
                state.0.insert(
                    op.id,
                    Row {
                        table,
                        a,
                        b,
                        c,
                        stamp,
                        deleted: false,
                    },
                );
            }
            // Deletion is permanent; field edits are last-writer-wins. The
            // window holds the row only if it is stored in the named table.
            Action::SetA(_, a) => {
                let row = state.0.get_mut(&op.entity).ok_or(Invalid)?;
                if stamp > row.stamp {
                    row.a = a;
                    row.stamp = stamp;
                }
            }
            Action::Delete(_) => state.0.get_mut(&op.entity).ok_or(Invalid)?.deleted = true,
        }
        Ok(())
    }
}
fn key(id: OperationId) -> [SqlValue; 2] {
    [
        SqlValue::Blob(id.node.0.to_vec()),
        SqlValue::Integer(id.sequence.get() as i64),
    ]
}
fn stored(id: OperationId, row: &Row) -> Vec<SqlValue> {
    let mut values = key(id).to_vec();
    values.extend([
        SqlValue::Integer(row.a),
        SqlValue::Integer(row.b),
        SqlValue::Text(row.c.clone()),
        SqlValue::Integer(row.stamp.lamport.get() as i64),
        SqlValue::Blob(row.stamp.node.0.to_vec()),
        SqlValue::Integer(row.stamp.sequence.get() as i64),
        SqlValue::Integer(i64::from(row.deleted)),
    ]);
    values
}
fn node(bytes: &[u8]) -> Result<NodeId, Invalid> {
    Ok(NodeId(bytes.try_into().map_err(|_| Invalid)?))
}
fn sequence(value: i64) -> Result<AuthorSequence, Invalid> {
    AuthorSequence::new(value as u64).map_err(|_| Invalid)
}
impl CompiledSchema for WideSchema {
    fn schema(&self) -> &'static [&'static str] {
        &self.0.schema
    }
    fn projection_query(&self) -> &'static str {
        self.0.projection
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        &self.0.names
    }
}
impl StoredApplication for WideSchema {
    type Row = (u16, OperationId, i64);
    fn descriptor(&self) -> ApplicationDescriptor {
        ApplicationDescriptor {
            definition: "wide-probe.v2",
            operations: Versions::V1,
            projection: 1,
        }
    }
    /// Every physical row shape has a real manifest entity and fields.
    fn manifest(&self) -> Option<ApplicationManifest> {
        let mut manifest = ApplicationManifest {
            publisher: [0x57; 32],
            name: "wide-probe".into(),
            app: Self::app_id(),
            schema: 1,
            projection: 1,
            entities: Vec::new(),
            fields: Vec::new(),
            operations: Vec::new(),
            domains: (1..=self.1)
                .map(|domain| DomainRule {
                    domain: domain_id(domain),
                    author_log: AuthorLogScope::Domain,
                    rule: "probe".into(),
                })
                .collect(),
        };
        for (kind, name) in [
            ("entity", "Row"),
            ("field", "a"),
            ("operation", "Create"),
            ("operation", "SetA"),
            ("operation", "Delete"),
        ] {
            let item = DefinitionItem {
                id: manifest.item_id(kind, name),
                name: name.into(),
                shape: "probe".into(),
            };
            match kind {
                "entity" => manifest.entities.push(item),
                "field" => manifest.fields.push(item),
                _ => manifest.operations.push(item),
            }
        }
        manifest.entities.clear();
        manifest.fields.clear();
        for table in 0..self.tables() {
            let name = format!("t{table}");
            manifest.entities.push(DefinitionItem {
                id: manifest.item_id("entity", &name),
                name: name.clone(),
                shape: "row".into(),
            });
            for column in ["a", "b", "c"] {
                let name = format!("{name}.{column}");
                manifest.fields.push(DefinitionItem {
                    id: manifest.item_id("field", &name),
                    name,
                    shape: "value".into(),
                });
            }
        }
        Some(manifest)
    }
    fn projection_rows(&self, state: &Rows) -> Vec<Vec<SqlValue>> {
        let mut rows: Vec<_> = state
            .0
            .iter()
            .map(|(id, row)| (row.table, *id, row))
            .collect();
        rows.sort_by_key(|(table, id, _)| (*table, *id));
        rows.into_iter()
            .map(|(table, id, row)| {
                let mut values = vec![SqlValue::Integer(i64::from(table))];
                values.extend(stored(id, row));
                values
            })
            .collect()
    }
    fn touched(&self, op: &Envelope) -> Vec<ProjectionStatement> {
        match decode(&op.payload) {
            Ok(action @ (Action::SetA(..) | Action::Delete(_))) => self
                .0
                .select
                .get(usize::from(action.table()))
                .map(|sql| ProjectionStatement {
                    sql: (*sql).into(),
                    values: key(op.entity).to_vec(),
                })
                .into_iter()
                .collect(),
            // A created row is new; an invalid payload fails in the reducer.
            _ => Vec::new(),
        }
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Rows, Invalid> {
        let mut state = Rows::default();
        for row in rows {
            let [
                SqlValue::Integer(table),
                SqlValue::Blob(id_node),
                SqlValue::Integer(id_sequence),
                SqlValue::Integer(a),
                SqlValue::Integer(b),
                SqlValue::Text(c),
                SqlValue::Integer(lamport),
                SqlValue::Blob(stamp_node),
                SqlValue::Integer(stamp_sequence),
                SqlValue::Integer(deleted @ (0 | 1)),
            ] = row.as_slice()
            else {
                return Err(Invalid);
            };
            state.0.insert(
                OperationId {
                    node: node(id_node)?,
                    sequence: sequence(*id_sequence)?,
                },
                Row {
                    table: *table as u16,
                    a: *a,
                    b: *b,
                    c: c.clone(),
                    stamp: Stamp {
                        lamport: LamportTime::new(*lamport as u64).map_err(|_| Invalid)?,
                        node: node(stamp_node)?,
                        sequence: sequence(*stamp_sequence)?,
                    },
                    deleted: *deleted == 1,
                },
            );
        }
        Ok(state)
    }
    fn project(&self, state: &Rows, op: &Envelope) -> Vec<ProjectionStatement> {
        let row = &state.0[&op.entity];
        vec![ProjectionStatement {
            sql: self.0.upsert[usize::from(row.table)].into(),
            values: stored(op.entity, row),
        }]
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Invalid> {
        let mut live = Vec::new();
        for row in rows {
            match row.as_slice() {
                [
                    SqlValue::Integer(table),
                    SqlValue::Blob(id_node),
                    SqlValue::Integer(id_sequence),
                    SqlValue::Integer(a),
                    ..,
                    SqlValue::Integer(deleted),
                ] => {
                    if *deleted == 0 {
                        live.push((
                            *table as u16,
                            OperationId {
                                node: node(id_node)?,
                                sequence: sequence(*id_sequence)?,
                            },
                            *a,
                        ));
                    }
                }
                _ => return Err(Invalid),
            }
        }
        Ok(live)
    }
    fn read_views(
        &self,
        context: AdmissionContext,
        policy: &Policy,
    ) -> Result<Vec<ReadView>, Invalid> {
        self.admit_read(context.scope, context.caller, policy)?;
        Ok(self
            .0
            .names
            .iter()
            .zip(&self.0.views)
            .map(|(name, sql)| ReadView {
                name: (*name).into(),
                sql: (*sql).into(),
                values: Vec::new(),
                key: rocky_core::columns(&["node", "sequence"]),
                not_null: Vec::new(),
            })
            .collect())
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
}

pub fn domain_id(domain: u16) -> DomainId {
    if domain == 1 {
        return DomainId([1; 16]);
    }
    let mut bytes = [2; 16];
    bytes[..2].copy_from_slice(&domain.to_be_bytes());
    DomainId(bytes)
}
