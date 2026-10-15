//! Independent draft notes: a merge-only application of the trusted runtime.
use rocky_core::*;
use std::collections::BTreeMap;

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
        entities: Vec::new(),
        fields: Vec::new(),
        operations: Vec::new(),
        domains: [DomainId([1; 16]), DomainId([2; 16])]
            .into_iter()
            .map(|domain| DomainRule {
                domain,
                author_log: AuthorLogScope::Domain,
                rule: "independent; domain-scoped author logs; no cross-domain references".into(),
            })
            .collect(),
    };
    for (kind, name, shape) in [
        ("entity", "Draft", "operation-id"),
        ("field", "Note", "UTF-8<=1024; LWW lamport/node/sequence"),
        (
            "operation",
            "SetNote",
            "UTF-8<=1024; create or LWW edit; no stock mutation",
        ),
    ] {
        let item = DefinitionItem {
            id: manifest.item_id(kind, name),
            name: name.into(),
            shape: shape.into(),
        };
        match kind {
            "entity" => manifest.entities.push(item),
            "field" => manifest.fields.push(item),
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
        Ok(vec![drafts])
    }
}
