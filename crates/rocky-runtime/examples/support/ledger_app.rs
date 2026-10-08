use rocky_core::*;
use std::collections::BTreeMap;

#[derive(Clone, Default)]
pub struct State {
    values: BTreeMap<OperationId, (i64, PrincipalId)>,
    events: BTreeMap<OperationId, i64>,
}
#[derive(Clone, Copy)]
pub struct LedgerApp;
#[derive(Debug)]
pub struct Denied;
impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ledger transition denied")
    }
}
impl std::error::Error for Denied {}
impl Application for LedgerApp {
    type State = State;
    type Policy = (Scope, PrincipalId);
    type Error = Denied;
    fn validate(&self, op: &Envelope) -> Result<(), Denied> {
        op.validate_dependencies(op.payload.first() == Some(&0))
            .map_err(|_| Denied)?;
        if !matches!(op.payload.as_slice(), [0, _, ..] | [1, _, ..] | [2, _, ..])
            || op.payload.len() != 25
        {
            return Err(Denied);
        }
        Ok(())
    }
    fn admit(&self, op: &Envelope, policy: &Self::Policy) -> Result<(), Denied> {
        if op.scope != policy.0 || op.caller != policy.1 {
            return Err(Denied);
        }
        Ok(())
    }
    fn apply_ready(
        &self,
        state: &mut State,
        op: &Envelope,
        _: AdmissionBoundary,
    ) -> Result<(), Denied> {
        self.validate(op)?;
        let amount = i64::from_le_bytes(op.payload[1..9].try_into().map_err(|_| Denied)?);
        let owner = PrincipalId(op.payload[9..25].try_into().map_err(|_| Denied)?);
        match op.payload[0] {
            0 => {
                state.values.insert(op.entity, (amount, owner));
            }
            1 | 2 => {
                let value = state.values.get_mut(&op.entity).ok_or(Denied)?;
                value.0 = value.0.checked_add(amount).ok_or(Denied)?;
                if op.payload[0] == 2 {
                    value.1 = owner;
                }
            }
            _ => return Err(Denied),
        }
        state.events.insert(op.id, state.values[&op.entity].0);
        Ok(())
    }
    fn admit_candidate(
        &self,
        before: &State,
        after: &State,
        op: &Envelope,
        context: AdmissionContext,
        policy: &Self::Policy,
    ) -> Result<(), Denied> {
        self.admit(op, policy)?;
        // Both the selected row and its resulting ownership must authorize all fields.
        if context.policy_version != 1
            || before
                .values
                .get(&op.entity)
                .is_some_and(|(_, owner)| *owner != context.caller)
            || after
                .values
                .get(&op.entity)
                .is_none_or(|(value, owner)| *owner != context.caller || *value < 0)
        {
            return Err(Denied);
        }
        Ok(())
    }
}
fn value_row(kind: &str, id: OperationId, value: i64, owner: SqlValue) -> Vec<SqlValue> {
    vec![
        SqlValue::Text(kind.into()),
        SqlValue::Blob(id.node.0.to_vec()),
        SqlValue::Integer(id.sequence.get() as i64),
        SqlValue::Integer(value),
        owner,
    ]
}
impl CompiledSchema for LedgerApp {
    fn schema(&self) -> &'static [&'static str] {
        &[
            "CREATE TABLE accounts(node BLOB,sequence INTEGER,value INTEGER,owner BLOB,PRIMARY KEY(node,sequence))",
            "CREATE TABLE events(node BLOB,sequence INTEGER,value INTEGER,PRIMARY KEY(node,sequence))",
        ]
    }
    fn projection_query(&self) -> &'static str {
        "SELECT 'account',node,sequence,value,owner FROM accounts UNION ALL SELECT 'event',node,sequence,value,NULL FROM events ORDER BY 1,2,3"
    }
    fn checkpoint_tables(&self) -> &'static [&'static str] {
        &["accounts", "events"]
    }
}
impl StoredApplication for LedgerApp {
    type Row = (OperationId, i64);
    fn descriptor(&self) -> rocky_core::ApplicationDescriptor {
        rocky_core::ApplicationDescriptor {
            definition: "example.ledger.v1",
            operations: rocky_core::Versions::V1,
            projection: 1,
        }
    }
    fn projection_rows(&self, state: &State) -> Vec<Vec<SqlValue>> {
        state
            .values
            .iter()
            .map(|(id, (value, owner))| {
                value_row("account", *id, *value, SqlValue::Blob(owner.0.to_vec()))
            })
            .chain(
                state
                    .events
                    .iter()
                    .map(|(id, value)| value_row("event", *id, *value, SqlValue::Null)),
            )
            .collect()
    }
    /// The account an operation edits. Its event row is new and needs no load.
    fn touched(&self, op: &Envelope) -> Vec<ProjectionStatement> {
        vec![ProjectionStatement {
            sql: "SELECT 'account',node,sequence,value,owner FROM accounts WHERE node=? AND sequence=?".into(),
            values: vec![
                SqlValue::Blob(op.entity.node.0.to_vec()),
                SqlValue::Integer(op.entity.sequence.get() as i64),
            ],
        }]
    }
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<State, Denied> {
        let mut state = State::default();
        for row in rows {
            let [
                SqlValue::Text(kind),
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
                SqlValue::Integer(value),
                owner,
            ] = row.as_slice()
            else {
                return Err(Denied);
            };
            let id = OperationId {
                node: NodeId(node.as_slice().try_into().map_err(|_| Denied)?),
                sequence: AuthorSequence::new(*sequence as u64).map_err(|_| Denied)?,
            };
            match (kind.as_str(), owner) {
                ("account", SqlValue::Blob(owner)) => {
                    let owner = PrincipalId(owner.as_slice().try_into().map_err(|_| Denied)?);
                    state.values.insert(id, (*value, owner));
                }
                ("event", SqlValue::Null) => {
                    state.events.insert(id, *value);
                }
                _ => return Err(Denied),
            }
        }
        Ok(state)
    }
    fn project(&self, state: &State, op: &Envelope) -> Vec<ProjectionStatement> {
        let (value, owner) = state.values[&op.entity];
        vec![
            ProjectionStatement {
                sql: "INSERT INTO accounts VALUES(?,?,?,?) ON CONFLICT(node,sequence) DO UPDATE SET value=excluded.value,owner=excluded.owner".into(),
                values: vec![
                    SqlValue::Blob(op.entity.node.0.to_vec()),
                    SqlValue::Integer(op.entity.sequence.get() as i64),
                    SqlValue::Integer(value),
                    SqlValue::Blob(owner.0.to_vec()),
                ],
            },
            ProjectionStatement {
                sql: "INSERT INTO events VALUES(?,?,?)".into(),
                values: vec![
                    SqlValue::Blob(op.id.node.0.to_vec()),
                    SqlValue::Integer(op.id.sequence.get() as i64),
                    SqlValue::Integer(value),
                ],
            },
        ]
    }
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Denied> {
        let mut visible = Vec::new();
        for row in rows {
            let [
                SqlValue::Text(kind),
                SqlValue::Blob(node),
                SqlValue::Integer(sequence),
                SqlValue::Integer(value),
                _,
            ] = row.as_slice()
            else {
                return Err(Denied);
            };
            if kind == "account" {
                visible.push((
                    OperationId {
                        node: NodeId(node.as_slice().try_into().map_err(|_| Denied)?),
                        sequence: AuthorSequence::new(*sequence as u64).map_err(|_| Denied)?,
                    },
                    *value,
                ));
            }
        }
        Ok(visible)
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Self::Policy,
    ) -> Result<(), Denied> {
        if (scope, caller) != *policy {
            return Err(Denied);
        }
        Ok(())
    }
}
pub fn payload(kind: u8, amount: i64, owner: PrincipalId) -> Vec<u8> {
    let mut bytes = vec![kind];
    bytes.extend(amount.to_le_bytes());
    bytes.extend(owner.0);
    bytes
}
