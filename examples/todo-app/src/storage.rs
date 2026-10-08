use crate::{Error, StaticPolicy, TodoApp, TodoEntry, TodoState, VisibleRow};
use rocky_core::{
    AuthorSequence, CompiledSchema, Envelope, NodeId, OperationId, PrincipalId,
    ProjectionStatement, Scope, SqlValue, Stamp, StoredApplication,
};

const SELECT: &str = "SELECT node,sequence,title,completed,title_stamp,completed_stamp,tombstone FROM todos ORDER BY node,sequence";
const TOUCHED: &str = "SELECT node,sequence,title,completed,title_stamp,completed_stamp,tombstone FROM todos WHERE node=? AND sequence=?";
const UPSERT: &str = "INSERT INTO todos(node,sequence,title,completed,title_stamp,completed_stamp,tombstone) VALUES(?,?,?,?,?,?,?) ON CONFLICT(node,sequence) DO UPDATE SET title=excluded.title,completed=excluded.completed,title_stamp=excluded.title_stamp,completed_stamp=excluded.completed_stamp,tombstone=excluded.tombstone";

fn stamp(value: Stamp) -> SqlValue {
    let mut bytes = value.lamport.get().to_le_bytes().to_vec();
    bytes.extend(value.node.0);
    bytes.extend(value.sequence.get().to_le_bytes());
    SqlValue::Blob(bytes)
}
fn stored_stamp(value: &SqlValue) -> Result<Stamp, Error> {
    let SqlValue::Blob(bytes) = value else {
        return Err(Error::InvalidProjection);
    };
    let field = |range: std::ops::Range<usize>| bytes.get(range).ok_or(Error::InvalidProjection);
    let number = |range| -> Result<u64, Error> {
        Ok(u64::from_le_bytes(
            field(range)?
                .try_into()
                .map_err(|_| Error::InvalidProjection)?,
        ))
    };
    if bytes.len() != 32 {
        return Err(Error::InvalidProjection);
    }
    Ok(Stamp {
        lamport: rocky_core::LamportTime::new(number(0..8)?)?,
        node: NodeId(
            field(8..24)?
                .try_into()
                .map_err(|_| Error::InvalidProjection)?,
        ),
        sequence: AuthorSequence::new(number(24..32)?)?,
    })
}
fn row(id: OperationId, entry: &TodoEntry) -> Vec<SqlValue> {
    let mut values = vec![
        SqlValue::Blob(id.node.0.to_vec()),
        SqlValue::Integer(id.sequence.get() as i64),
    ];
    match entry {
        TodoEntry::Live { title, completed } => values.extend([
            SqlValue::Text(title.value.clone()),
            SqlValue::Integer(i64::from(completed.value)),
            stamp(title.stamp),
            stamp(completed.stamp),
            SqlValue::Integer(0),
        ]),
        TodoEntry::Tombstone => values.extend([
            SqlValue::Null,
            SqlValue::Null,
            SqlValue::Null,
            SqlValue::Null,
            SqlValue::Integer(1),
        ]),
    }
    values
}
impl CompiledSchema for TodoApp {
    fn schema(&self) -> &'static [&'static str] {
        &[
            "CREATE TABLE todos(node BLOB NOT NULL,sequence INTEGER NOT NULL,title TEXT,completed INTEGER,title_stamp BLOB,completed_stamp BLOB,tombstone INTEGER NOT NULL,PRIMARY KEY(node,sequence))",
        ]
    }
    fn projection_query(&self) -> &'static str {
        SELECT
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        &["todos"]
    }
}
impl StoredApplication for TodoApp {
    type Row = VisibleRow;
    fn descriptor(&self) -> rocky_core::ApplicationDescriptor {
        rocky_core::ApplicationDescriptor {
            definition: "rocky.todo.definition.v1",
            operations: rocky_core::Versions::V1,
            projection: 1,
        }
    }
    fn commands(&self) -> &'static [rocky_core::CommandMethod] {
        &[rocky_core::CommandMethod {
            name: crate::PUBLISH_LIST_SNAPSHOT,
            version: 1,
            schema: 1,
            handler: 1,
        }]
    }
    fn command(&self, method: &str) -> Option<&dyn rocky_core::CommandHandler<Self>> {
        (method == crate::PUBLISH_LIST_SNAPSHOT).then_some(self)
    }
    fn projection_rows(&self, state: &TodoState) -> Vec<Vec<SqlValue>> {
        state
            .entries
            .iter()
            .map(|(id, entry)| row(*id, entry))
            .collect()
    }
    fn touched(&self, operation: &Envelope) -> Vec<ProjectionStatement> {
        vec![ProjectionStatement {
            sql: TOUCHED.into(),
            values: vec![
                SqlValue::Blob(operation.entity.node.0.to_vec()),
                SqlValue::Integer(operation.entity.sequence.get() as i64),
            ],
        }]
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<TodoState, Error> {
        let mut state = TodoState::default();
        for values in rows {
            let [
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
                title,
                completed,
                title_stamp,
                completed_stamp,
                SqlValue::Integer(deleted),
            ] = values.as_slice()
            else {
                return Err(Error::InvalidProjection);
            };
            let id = OperationId {
                node: NodeId(
                    node.as_slice()
                        .try_into()
                        .map_err(|_| Error::InvalidProjection)?,
                ),
                sequence: AuthorSequence::new(*sequence as u64)?,
            };
            let entry = match (deleted, title, completed) {
                (1, SqlValue::Null, SqlValue::Null) => TodoEntry::Tombstone,
                (0, SqlValue::Text(title), SqlValue::Integer(completed @ (0 | 1))) => {
                    TodoEntry::Live {
                        title: crate::Register {
                            value: title.clone(),
                            stamp: stored_stamp(title_stamp)?,
                        },
                        completed: crate::Register {
                            value: *completed == 1,
                            stamp: stored_stamp(completed_stamp)?,
                        },
                    }
                }
                _ => return Err(Error::InvalidProjection),
            };
            state.entries.insert(id, entry);
        }
        Ok(state)
    }
    fn project(&self, state: &TodoState, operation: &Envelope) -> Vec<ProjectionStatement> {
        state
            .entry(operation.entity)
            .map(|entry| ProjectionStatement {
                sql: UPSERT.into(),
                values: row(operation.entity, entry),
            })
            .into_iter()
            .collect()
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<VisibleRow>, Error> {
        let mut visible = Vec::new();
        for values in rows {
            let [
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
                title,
                completed,
                _,
                _,
                SqlValue::Integer(deleted),
            ] = values.as_slice()
            else {
                return Err(Error::InvalidProjection);
            };
            let id = OperationId {
                node: NodeId(
                    node.as_slice()
                        .try_into()
                        .map_err(|_| Error::InvalidProjection)?,
                ),
                sequence: AuthorSequence::new(*sequence as u64)?,
            };
            match (deleted, title, completed) {
                (1, SqlValue::Null, SqlValue::Null) => {}
                (0, SqlValue::Text(title), SqlValue::Integer(completed @ (0 | 1))) => {
                    visible.push(VisibleRow {
                        id,
                        title: title.clone(),
                        completed: *completed == 1,
                    })
                }
                _ => return Err(Error::InvalidProjection),
            }
        }
        Ok(visible)
    }
    fn select_rows(
        &self,
        rows: Vec<VisibleRow>,
        context: rocky_core::AdmissionContext,
        policy: &StaticPolicy,
    ) -> Result<Vec<VisibleRow>, Error> {
        policy.admit_read(context.scope, context.caller)?;
        Ok(rows
            .into_iter()
            .filter(|row| {
                policy.rows.as_ref().is_none_or(|grants| {
                    grants
                        .iter()
                        .any(|grant| grant.entity == row.id && grant.read)
                })
            })
            .collect())
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &StaticPolicy,
    ) -> Result<(), Error> {
        policy.admit_read(scope, caller)
    }
}

impl rocky_core::CommandHandler<TodoApp> for TodoApp {
    fn seal(&self, state: &TodoState) -> Result<Vec<u8>, Error> {
        crate::encode_snapshot(&state.visible_rows())
    }
    fn admit(
        &self,
        request: &rocky_core::StrongRequest,
        policy: &StaticPolicy,
    ) -> Result<(), Error> {
        <Self as rocky_core::CommandHandler<TodoApp>>::validate(self, request)?;
        policy.admit_read(request.scope, request.caller)?;
        if !policy.publish
            || !policy.nodes.contains(&request.id.node)
            || request.method != "PublishListSnapshot"
            || request.version != 1
            || request.schema != 1
            || request.handler != 1
        {
            return Err(Error::DeniedCommand);
        }
        Ok(())
    }
    fn validate(&self, request: &rocky_core::StrongRequest) -> Result<(), Error> {
        if !request.arguments.is_empty()
            || request.policy != [0; 32]
            || request.method != crate::PUBLISH_LIST_SNAPSHOT
            || request.version != 1
            || request.schema != 1
            || request.handler != 1
        {
            return Err(Error::DeniedCommand);
        }
        Ok(())
    }
    fn select_result(
        &self,
        bytes: &[u8],
        context: rocky_core::AdmissionContext,
        policy: &StaticPolicy,
    ) -> Result<bool, Error> {
        policy.admit_read(context.scope, context.caller)?;
        let rows = crate::decode_snapshot(bytes)?;
        Ok(rows.iter().all(|row| {
            policy.rows.as_ref().is_none_or(|grants| {
                grants
                    .iter()
                    .any(|grant| grant.entity == row.id && grant.read)
            })
        }))
    }
    fn execute(
        &self,
        state: &TodoState,
        _request: &rocky_core::StrongRequest,
    ) -> Result<Vec<u8>, Error> {
        self.seal(state)
    }
}
