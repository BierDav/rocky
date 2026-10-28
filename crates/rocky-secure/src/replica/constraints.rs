//! Unique indexes and foreign keys of mergeable tables. Each constrained
//! table keeps its key winners in a private candidate table; the typed table
//! holds the visible candidates as a pure function of them.
use super::typed::{Projected, Typed, quoted};
use super::*;
use rocky_core::{ConflictPolicy, DELETE_CASCADE, LogicalSchema};
use std::collections::BTreeMap;

type DesiredRows =
    BTreeMap<(usize, ResourceScope, Commitment), (Vec<SqlValue>, Option<Vec<SqlValue>>)>;

const LIVE: &str = "\"rocky_live\"";
/// Local bookkeeping of the support rows in typed tables.
pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_support(tab TEXT NOT NULL,scope BLOB NOT NULL,id INTEGER NOT NULL,PRIMARY KEY(tab,scope,id)) WITHOUT ROWID",
];
const STAMPS: &str = "\"rocky_stamps\"";

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Identity {
    scope: ResourceScope,
    id: i64,
}
impl Identity {
    fn unpartitioned(node: &Node, id: i64) -> Self {
        Self {
            scope: node.scope,
            id,
        }
    }
}

struct Unique {
    columns: Vec<usize>,
    fields: Vec<Commitment>,
    policy: ConflictPolicy,
    select: String,
}
struct Reference {
    columns: Vec<usize>,
    parent: usize,
    parent_columns: Vec<usize>,
    identity: usize,
    cascade: bool,
    /// `NOT NULL`: a support row points it at the parent's virtual row.
    required: bool,
    /// Candidates of this table whose reference names an `id`.
    select: String,
}
/// Statements of a table that may hold puppets or a virtual row.
struct Support {
    /// The typed table's SQL name, as the support bookkeeping names it.
    name: String,
    /// Inserts a support row: `id`, then `row_kind`.
    insert: String,
    delete: String,
    kind: String,
    /// The column values of a support row; `id` and `row_kind` are bound.
    values: Vec<SqlValue>,
    id: usize,
    row_kind: usize,
}
/// One constrained table: its typed projection and candidate table.
pub(super) struct Node {
    pub(super) scope: ResourceScope,
    /// Liveness of its rows comes from its existence log alone.
    logged: bool,
    support: Option<Support>,
    /// Position in `Typed::tables`.
    table: usize,
    columns: usize,
    partition: Vec<usize>,
    partition_filter: String,
    key: Vec<usize>,
    id: usize,
    uniques: Vec<Unique>,
    references: Vec<Reference>,
    /// `(node, reference)` of every foreign key that names this table.
    children: Vec<(usize, usize)>,
    pub(super) statements: Vec<String>,
    select_id: String,
    select_key: String,
    select_all: String,
    upsert: String,
    delete_key: String,
    clear: String,
    set_live: String,
}
impl Node {
    /// Whether a typed row of this table is a puppet or the virtual row.
    fn is_support(&self, values: &[SqlValue]) -> bool {
        self.support
            .as_ref()
            .is_some_and(|support| values.get(support.row_kind) != Some(&SqlValue::Integer(0)))
    }
    /// The support rows a typed row of this table (`node`) may need or free:
    /// its own `id`, and every row of an existence log it references.
    fn support_seeds(
        &self,
        node: usize,
        constraints: &Constraints,
        values: &[SqlValue],
    ) -> Vec<(usize, Identity)> {
        let mut seeds = Vec::new();
        if self.support.is_some()
            && let Some(SqlValue::Integer(id)) = values.get(self.id)
        {
            seeds.push((node, Identity::unpartitioned(self, *id)));
        }
        for reference in &self.references {
            if constraints.nodes[reference.parent].logged
                && let Some(SqlValue::Integer(id)) = values.get(reference.columns[0])
            {
                seeds.push((
                    reference.parent,
                    Identity::unpartitioned(&constraints.nodes[reference.parent], *id),
                ));
            }
        }
        seeds
    }
}
/// The constrained tables of an installation.
pub(super) struct Constraints {
    pub(super) nodes: Vec<Node>,
}

/// One key winner of a constrained table.
#[derive(Clone, Debug)]
struct Candidate {
    scope: ResourceScope,
    values: Vec<SqlValue>,
    /// Whether it was anchored when last resolved; changes propagate.
    live: bool,
    /// Per unique index: the latest stamp of its columns.
    stamps: Vec<Option<Stamp>>,
}
impl Candidate {
    fn id(&self, node: &Node) -> Option<Identity> {
        match self.values.get(node.id) {
            Some(SqlValue::Integer(id)) => Some(Identity {
                scope: self.scope,
                id: *id,
            }),
            _ => None,
        }
    }
    fn stamp(&self, unique: usize) -> Option<&Stamp> {
        self.stamps.get(unique).and_then(Option::as_ref)
    }
    fn tuple(&self, columns: &[usize]) -> Option<Vec<SqlValue>> {
        let values: Vec<SqlValue> = columns.iter().map(|i| self.values[*i].clone()).collect();
        (!values.contains(&SqlValue::Null)).then_some(values)
    }
}

/// Rows a change may affect, collected while candidates follow stored rows.
#[derive(Default)]
pub(super) struct Seeds {
    /// Candidates whose own state changed: always propagate.
    forced: BTreeSet<(usize, Identity)>,
    /// Candidates that may depend on a change.
    queued: BTreeSet<(usize, Identity)>,
    #[cfg(any(test, feature = "qualification"))]
    dependents: BTreeSet<(usize, Identity)>,
    /// Natural-key slots whose candidate changed, by row key.
    slots: BTreeMap<(usize, ResourceScope, Commitment), Vec<SqlValue>>,
    /// Typed `id`s whose support row may have to appear or go.
    support: BTreeSet<(usize, Identity)>,
}
impl Seeds {
    pub(super) fn is_empty(&self) -> bool {
        self.forced.is_empty()
            && self.queued.is_empty()
            && self.slots.is_empty()
            && self.support.is_empty()
    }
}

fn corrupt(reason: impl Into<String>) -> Error {
    Error::Corrupt(reason.into())
}
fn conflict(reason: String) -> Error {
    Error::Sql(crate::sql::SqlError::Capture(reason))
}

impl Constraints {
    /// The constrained tables of `schema`, or None when it has none.
    pub(super) fn new(schema: &LogicalSchema, tables: &[Projected]) -> Result<Option<Self>, Error> {
        let incompatible = |reason: String| Error::Incompatible(reason);
        let constrained: Vec<usize> = tables
            .iter()
            .enumerate()
            .filter(|(_, projected)| {
                schema
                    .table(&projected.table.sql_name)
                    .is_some_and(|table| schema.constrained(table))
            })
            .map(|(index, _)| index)
            .collect();
        if constrained.is_empty() {
            return Ok(None);
        }
        let node_of = |sql_name: &str| {
            constrained
                .iter()
                .position(|index| tables[*index].table.sql_name.eq_ignore_ascii_case(sql_name))
        };
        let mut nodes = Vec::new();
        for &index in &constrained {
            let projected = &tables[index];
            let table = schema.table(&projected.table.sql_name).ok_or_else(|| {
                incompatible(format!("{} is not in the schema", projected.table.sql_name))
            })?;
            let position = |name: &str| {
                table
                    .columns
                    .iter()
                    .position(|column| column.sql_name.eq_ignore_ascii_case(name))
                    .ok_or_else(|| {
                        incompatible(format!("{}.{name} is not a column", table.sql_name))
                    })
            };
            let name = quoted(&format!("secure_candidates_{}", table.sql_name));
            let index_name =
                |suffix: &str| quoted(&format!("secure_candidates_{}_{suffix}", table.sql_name));
            let columns: Vec<String> = table.columns.iter().map(|c| quoted(&c.sql_name)).collect();
            let list = |positions: &[usize]| {
                positions
                    .iter()
                    .map(|i| columns[*i].clone())
                    .collect::<Vec<_>>()
                    .join(",")
            };
            let filter = |positions: &[usize]| {
                positions
                    .iter()
                    .map(|i| format!("{}=?", columns[*i]))
                    .collect::<Vec<_>>()
                    .join(" AND ")
            };
            let key = projected.table.key.clone();
            let id = position("id")?;
            let partition = table
                .partition_by
                .iter()
                .map(|name| position(name))
                .collect::<Result<Vec<_>, _>>()?;
            let mut identity_key = partition.clone();
            if !identity_key.contains(&id) {
                identity_key.push(id);
            }
            let row_kind = position("row_kind")?;
            let supported = schema
                .support_tables()
                .iter()
                .any(|support| support.sql_name == table.sql_name);
            let select_all = format!("SELECT {},{LIVE},{STAMPS} FROM {name}", columns.join(","));
            // Declared collations make the lookups compare as the real index does.
            let definitions: Vec<String> = table
                .columns
                .iter()
                .map(|column| {
                    let affinity = format!("{:?}", column.affinity).to_ascii_uppercase();
                    let collation = format!("{:?}", column.collation).to_ascii_uppercase();
                    format!(
                        "{} {affinity} COLLATE {collation}",
                        quoted(&column.sql_name)
                    )
                })
                .collect();
            let mut statements = vec![
                format!(
                    "CREATE TABLE IF NOT EXISTS {name}({},{LIVE} INTEGER NOT NULL,{STAMPS} BLOB NOT NULL,PRIMARY KEY({}))",
                    definitions.join(","),
                    list(&key)
                ),
                format!(
                    "CREATE UNIQUE INDEX IF NOT EXISTS {} ON {name}({})",
                    index_name("id"),
                    list(&identity_key)
                ),
            ];
            let mut uniques = Vec::new();
            for (number, (index, policy)) in table.policy_indexes().enumerate() {
                let positions = index
                    .columns
                    .iter()
                    .map(|column| position(&column.name))
                    .collect::<Result<Vec<_>, _>>()?;
                statements.push(format!(
                    "CREATE INDEX IF NOT EXISTS {} ON {name}({})",
                    index_name(&format!("u{number}")),
                    list(&positions)
                ));
                uniques.push(Unique {
                    fields: positions
                        .iter()
                        .map(|i| projected.table.fields[*i])
                        .collect(),
                    select: format!("{select_all} WHERE {}", filter(&positions)),
                    columns: positions,
                    policy,
                });
            }
            let mut references = Vec::new();
            for (number, key) in table.foreign_keys.iter().enumerate() {
                let positions = key
                    .columns
                    .iter()
                    .map(|column| position(column))
                    .collect::<Result<Vec<_>, _>>()?;
                let parent = node_of(&key.table).ok_or_else(|| {
                    incompatible(format!("{} is not a constrained table", key.table))
                })?;
                let parent_table = schema
                    .table(&key.table)
                    .ok_or_else(|| incompatible("missing parent".into()))?;
                let parent_columns = key
                    .references
                    .iter()
                    .map(|name| {
                        parent_table
                            .columns
                            .iter()
                            .position(|column| column.sql_name.eq_ignore_ascii_case(name))
                            .ok_or_else(|| incompatible("missing parent column".into()))
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let identity = key
                    .references
                    .iter()
                    .position(|name| name.eq_ignore_ascii_case("id"))
                    .ok_or_else(|| {
                        incompatible("reference must name the incarnation identity".into())
                    })?;
                // Group references carry the complete parent partition.
                let logged = schema
                    .table(&key.table)
                    .is_some_and(|parent| parent.existence_log);
                if !logged
                    && parent_table.partition_by.is_empty()
                    && tables[constrained[parent]].scope.encryption != projected.scope.encryption
                {
                    return Err(incompatible(format!(
                        "{}.{} references {} in another reader scope",
                        table.sql_name, key.columns[0], key.table
                    )));
                }
                statements.push(format!(
                    "CREATE INDEX IF NOT EXISTS {} ON {name}({})",
                    index_name(&format!("r{number}")),
                    list(&positions)
                ));
                references.push(Reference {
                    select: format!("{select_all} WHERE {}", filter(&positions)),
                    required: positions
                        .iter()
                        .any(|i| !partition.contains(i) && table.columns[*i].not_null),
                    parent_columns,
                    identity,
                    columns: positions,
                    parent,
                    cascade: key.on_delete == DELETE_CASCADE,
                });
            }
            let registers: Vec<String> = (0..columns.len())
                .filter(|i| !key.contains(i))
                .map(|i| format!("{0}=excluded.{0}", columns[i]))
                .chain([LIVE, STAMPS].map(|column| format!("{column}=excluded.{column}")))
                .collect();
            let support = if supported {
                let typed_name = quoted(&table.sql_name);
                let required: BTreeSet<usize> = references
                    .iter()
                    .filter(|reference| reference.required)
                    .flat_map(|reference| reference.columns.iter().copied())
                    .collect();
                let values = table
                    .columns
                    .iter()
                    .enumerate()
                    .map(|(index, column)| {
                        if required.contains(&index) {
                            SqlValue::Integer(0)
                        } else if !column.not_null {
                            SqlValue::Null
                        } else {
                            match column.affinity {
                                rocky_core::Affinity::Text => SqlValue::Text(String::new()),
                                rocky_core::Affinity::Blob => SqlValue::Blob(Vec::new()),
                                rocky_core::Affinity::Real => {
                                    SqlValue::real(0.0).unwrap_or(SqlValue::Integer(0))
                                }
                                _ => SqlValue::Integer(0),
                            }
                        }
                    })
                    .collect();
                // Generated read-only guard, visible in the schema.
                let guard = |event: &str, image: &str| {
                    format!(
                        "CREATE TRIGGER IF NOT EXISTS {} BEFORE {event} ON {typed_name} FOR EACH ROW WHEN {image}.\"row_kind\"<>0 AND NOT {}() BEGIN SELECT RAISE(ABORT,'puppets and virtual rows are read-only'); END",
                        quoted(&format!(
                            "secure_support_{}_{}",
                            table.sql_name,
                            event.to_ascii_lowercase()
                        )),
                        crate::sqlite::SUPPORT_MAINTENANCE
                    )
                };
                statements.push(guard("INSERT", "NEW"));
                statements.push(guard("UPDATE", "OLD"));
                statements.push(guard("DELETE", "OLD"));
                Some(Support {
                    name: table.sql_name.clone(),
                    insert: format!(
                        "INSERT INTO {typed_name}({}) VALUES({})",
                        columns.join(","),
                        vec!["?"; columns.len()].join(",")
                    ),
                    delete: format!(
                        "DELETE FROM {typed_name} WHERE {} AND \"row_kind\"<>0",
                        filter(&identity_key)
                    ),
                    kind: format!(
                        "SELECT \"row_kind\" FROM {typed_name} WHERE {}",
                        filter(&identity_key)
                    ),
                    values,
                    id,
                    row_kind,
                })
            } else {
                None
            };
            nodes.push(Node {
                scope: projected.scope,
                logged: table.existence_log,
                support,
                table: index,
                columns: columns.len(),
                partition_filter: filter(&partition),
                partition,
                select_id: format!("{select_all} WHERE {}", filter(&identity_key)),
                select_key: format!("{select_all} WHERE {}", filter(&key)),
                upsert: format!(
                    "INSERT INTO {name}({},{LIVE},{STAMPS}) VALUES({}) ON CONFLICT({}) DO UPDATE SET {}",
                    columns.join(","),
                    vec!["?"; columns.len() + 2].join(","),
                    list(&key),
                    registers.join(",")
                ),
                delete_key: format!("DELETE FROM {name} WHERE {}", filter(&key)),
                clear: format!("DELETE FROM {name}"),
                set_live: format!("UPDATE {name} SET {LIVE}=? WHERE {}", filter(&identity_key)),
                select_all,
                statements,
                key,
                id,
                uniques,
                references,
                children: Vec::new(),
            });
        }
        for child in 0..nodes.len() {
            for reference in 0..nodes[child].references.len() {
                let parent = nodes[child].references[reference].parent;
                nodes[parent].children.push((child, reference));
            }
        }
        Ok(Some(Self { nodes }))
    }
    pub(super) fn node(&self, scope: ResourceScope) -> Option<usize> {
        self.nodes.iter().position(|node| {
            node.scope == scope
                || (!node.partition.is_empty()
                    && node.scope.domain == scope.domain
                    && node.scope.table == scope.table)
        })
    }
    pub(super) fn statements(&self) -> impl Iterator<Item = &String> {
        self.nodes.iter().flat_map(|node| &node.statements)
    }
}

/// One resolution over the candidates as they stand; every answer is a pure
/// function of them, so results are cached for the pass.
struct Pass<'a, A> {
    inner: &'a Inner<A>,
    constraints: &'a Constraints,
    rows: BTreeMap<(usize, Identity), Option<Candidate>>,
    anchored: BTreeMap<(usize, Identity), bool>,
    visible: BTreeMap<(usize, Identity), bool>,
    loses: BTreeMap<(usize, Identity, usize), bool>,
    alive: BTreeMap<(usize, Identity), bool>,
    #[cfg(any(test, feature = "qualification"))]
    dependents: BTreeSet<(usize, Identity)>,
}

impl<'a, A: RowApplication> Pass<'a, A> {
    fn new(inner: &'a Inner<A>, constraints: &'a Constraints) -> Self {
        Self {
            inner,
            constraints,
            rows: BTreeMap::new(),
            anchored: BTreeMap::new(),
            visible: BTreeMap::new(),
            loses: BTreeMap::new(),
            alive: BTreeMap::new(),
            #[cfg(any(test, feature = "qualification"))]
            dependents: BTreeSet::new(),
        }
    }
    /// What the existence log of a logged table says about `id`.
    async fn alive(&mut self, node: usize, id: Identity) -> Result<bool, Error> {
        if let Some(alive) = self.alive.get(&(node, id)) {
            return Ok(*alive);
        }
        let alive = self.inner.alive(id.scope, id.id).await?;
        self.alive.insert((node, id), alive);
        Ok(alive)
    }
    fn node(&self, node: usize) -> &'a Node {
        &self.constraints.nodes[node]
    }
    async fn query(
        &mut self,
        node: usize,
        sql: &str,
        params: Vec<SqlValue>,
    ) -> Result<Vec<Candidate>, Error> {
        let width = self.node(node).columns;
        let found = collect_rows(
            self.inner
                .conn
                .query(sql.to_string(), bindings(params))
                .await?,
        )
        .await?;
        let mut candidates = Vec::with_capacity(found.len());
        for mut row in found {
            let (Some(SqlValue::Blob(stamps)), Some(SqlValue::Integer(live))) =
                (row.pop(), row.pop())
            else {
                return Err(corrupt("candidate row"));
            };
            if row.len() != width {
                return Err(corrupt("candidate width"));
            }
            let typed = self
                .inner
                .workspace
                .typed
                .as_ref()
                .ok_or_else(|| corrupt("typed constraints"))?;
            let scope = typed.row_scope(&typed.tables[self.node(node).table], &row)?;
            let candidate = Candidate {
                scope,
                values: row,
                live: live != 0,
                stamps: decode(&stamps)?,
            };
            if let Some(id) = candidate.id(self.node(node)) {
                self.rows.insert((node, id), Some(candidate.clone()));
            }
            candidates.push(candidate);
        }
        Ok(candidates)
    }
    async fn get(&mut self, node: usize, id: Identity) -> Result<Option<Candidate>, Error> {
        if let Some(found) = self.rows.get(&(node, id)) {
            return Ok(found.clone());
        }
        if !self.node(node).partition.is_empty() && !self.inner.partitions.contains_key(&id.scope) {
            return Ok(None);
        }
        let sql = self.node(node).select_id.clone();
        let found = self
            .query(node, &sql, self.identity_params(node, id)?)
            .await?
            .pop();
        self.rows.insert((node, id), found.clone());
        Ok(found)
    }
    fn partition_params(&self, node: usize, scope: ResourceScope) -> Result<Vec<SqlValue>, Error> {
        let spec = self.node(node);
        if spec.partition.is_empty() {
            return Ok(Vec::new());
        }
        let typed = self
            .inner
            .workspace
            .typed
            .as_ref()
            .ok_or_else(|| corrupt("typed constraints"))?;
        let tuple = self
            .inner
            .partitions
            .get(&scope)
            .ok_or_else(|| corrupt("missing constraint partition"))?;
        spec.partition
            .iter()
            .map(|index| {
                tuple
                    .values()
                    .get(&typed.tables[spec.table].table.fields[*index])
                    .ok_or_else(|| corrupt("missing constraint partition column"))?
                    .value()
                    .map_err(|error| corrupt(error.to_string()))
            })
            .collect()
    }
    fn identity_params(&self, node: usize, id: Identity) -> Result<Vec<SqlValue>, Error> {
        let mut params = self.partition_params(node, id.scope)?;
        if !self.node(node).partition.contains(&self.node(node).id) {
            params.push(SqlValue::Integer(id.id));
        }
        Ok(params)
    }
    /// The parent a reference names, or None for NULL.
    fn parent(
        &self,
        candidate: &Candidate,
        reference: &Reference,
    ) -> Result<Option<Identity>, Error> {
        if candidate.tuple(&reference.columns).is_none() {
            return Ok(None);
        }
        let Some(SqlValue::Integer(id)) =
            candidate.values.get(reference.columns[reference.identity])
        else {
            return Err(corrupt("reference identity"));
        };
        let parent = self.node(reference.parent);
        let typed = self
            .inner
            .workspace
            .typed
            .as_ref()
            .ok_or_else(|| corrupt("typed constraints"))?;
        let mut values = vec![SqlValue::Null; parent.columns];
        for (source, target) in reference.columns.iter().zip(&reference.parent_columns) {
            values[*target] = candidate.values[*source].clone();
        }
        Ok(Some(Identity {
            scope: typed.row_scope(&typed.tables[parent.table], &values)?,
            id: *id,
        }))
    }
    /// The candidates `CASCADE` references reach, the row itself first;
    /// None when one of them names a parent that is not a candidate. A
    /// table with an existence log ends the walk: its log alone decides
    /// whether its row is alive, on every device alike.
    async fn ancestors(
        &mut self,
        node: usize,
        id: Identity,
    ) -> Result<Option<Vec<(usize, Identity)>>, Error> {
        if self.node(node).logged && !self.alive(node, id).await? {
            return Ok(None);
        }
        let mut seen = BTreeSet::from([(node, id)]);
        let mut order = vec![(node, id)];
        let mut next = 0;
        while let Some(&(at, row)) = order.get(next) {
            next += 1;
            let Some(candidate) = self.get(at, row).await? else {
                return Ok(None);
            };
            for reference in &self.node(at).references {
                if let Some(parent) = self.parent(&candidate, reference)?
                    && !self.node(reference.parent).partition.is_empty()
                    && (!self.inner.partitions.contains_key(&parent.scope)
                        || !self
                            .inner
                            .opening_keys()
                            .any(|key| key.scope == parent.scope)
                        || !self.inner.holds(parent.scope).await?)
                {
                    return Ok(None);
                }
            }
            for reference in self.node(at).references.iter().filter(|r| r.cascade) {
                let logged = self.node(reference.parent).logged;
                match self.parent(&candidate, reference)? {
                    None => {}
                    Some(parent) if logged => {
                        if !self.alive(reference.parent, parent).await? {
                            return Ok(None);
                        }
                    }
                    Some(parent) if seen.insert((reference.parent, parent)) => {
                        order.push((reference.parent, parent));
                    }
                    Some(_) => {}
                }
            }
        }
        Ok(Some(order))
    }
    async fn anchored(&mut self, node: usize, id: Identity) -> Result<bool, Error> {
        if let Some(anchored) = self.anchored.get(&(node, id)) {
            return Ok(*anchored);
        }
        let reached = self.ancestors(node, id).await?;
        // Everything an anchored row reaches is anchored too.
        for row in reached.iter().flatten() {
            self.anchored.insert(*row, true);
        }
        self.anchored.insert((node, id), reached.is_some());
        Ok(reached.is_some())
    }
    /// Whether an anchored candidate with equal values has a later stamp.
    async fn loses(&mut self, node: usize, id: Identity, unique: usize) -> Result<bool, Error> {
        if let Some(loses) = self.loses.get(&(node, id, unique)) {
            return Ok(*loses);
        }
        let mut loses = false;
        if let Some(candidate) = self.get(node, id).await?
            && self.anchored(node, id).await?
        {
            let rule = &self.node(node).uniques[unique];
            if let Some(tuple) = candidate.tuple(&rule.columns) {
                let select = rule.select.clone();
                for member in self.query(node, &select, tuple).await? {
                    let Some(other) = member.id(self.node(node)) else {
                        continue;
                    };
                    // Equal stamps arise when one frame writes different
                    // columns of two rows; partition and id then break the tie.
                    let later = (member.stamp(unique), other) > (candidate.stamp(unique), id);
                    if other != id && later && self.anchored(node, other).await? {
                        loses = true;
                        break;
                    }
                }
            }
        }
        self.loses.insert((node, id, unique), loses);
        Ok(loses)
    }
    /// Anchored, and no row its `CASCADE` references reach loses a
    /// `LatestWins` index.
    async fn visible(&mut self, node: usize, id: Identity) -> Result<bool, Error> {
        if let Some(visible) = self.visible.get(&(node, id)) {
            return Ok(*visible);
        }
        let mut visible = false;
        if let Some(reached) = self.ancestors(node, id).await? {
            visible = true;
            'rows: for (at, row) in reached.iter().copied() {
                for unique in 0..self.node(at).uniques.len() {
                    if self.node(at).uniques[unique].policy == ConflictPolicy::LatestWins
                        && self.loses(at, row, unique).await?
                    {
                        visible = false;
                        break 'rows;
                    }
                }
            }
            if visible {
                for row in reached {
                    self.visible.insert(row, true);
                }
            }
        }
        self.visible.insert((node, id), visible);
        Ok(visible)
    }
    /// The typed row of a candidate, or None while it is hidden.
    async fn image(&mut self, node: usize, id: Identity) -> Result<Option<Vec<SqlValue>>, Error> {
        let Some(candidate) = self.get(node, id).await? else {
            return Ok(None);
        };
        if !self.visible(node, id).await? {
            return Ok(None);
        }
        let mut values = candidate.values.clone();
        for reference in self.node(node).references.iter().filter(|r| !r.cascade) {
            let shown = match self.parent(&candidate, reference)? {
                None => true,
                Some(parent) if self.node(reference.parent).logged => {
                    self.alive(reference.parent, parent).await?
                }
                Some(parent) => self.visible(reference.parent, parent).await?,
            };
            if !shown {
                for column in &reference.columns {
                    if !self.node(node).partition.contains(column) {
                        values[*column] = SqlValue::Null;
                    }
                }
            }
        }
        for unique in 0..self.node(node).uniques.len() {
            let rule = &self.node(node).uniques[unique];
            if rule.policy == ConflictPolicy::SetNull && self.loses(node, id, unique).await? {
                for column in &self.node(node).uniques[unique].columns {
                    values[*column] = SqlValue::Null;
                }
            }
        }
        Ok(Some(values))
    }
    /// Candidates whose state may follow this one's: equal unique values
    /// and every child that names it.
    async fn dependents(
        &mut self,
        node: usize,
        candidate: &Candidate,
    ) -> Result<Vec<(usize, Identity)>, Error> {
        let mut found = Vec::new();
        for unique in 0..self.node(node).uniques.len() {
            let rule = &self.node(node).uniques[unique];
            if let Some(tuple) = candidate.tuple(&rule.columns) {
                let select = rule.select.clone();
                for member in self.query(node, &select, tuple).await? {
                    found.extend(member.id(self.node(node)).map(|id| (node, id)));
                }
            }
        }
        if candidate.id(self.node(node)).is_some() {
            for (child, reference) in self.node(node).children.clone() {
                let rule = &self.node(child).references[reference];
                let select = rule.select.clone();
                let params = rule
                    .parent_columns
                    .iter()
                    .map(|column| candidate.values[*column].clone())
                    .collect();
                for row in self.query(child, &select, params).await? {
                    if let Some(id) = row.id(self.node(child)) {
                        #[cfg(any(test, feature = "qualification"))]
                        self.dependents.insert((child, id));
                        found.push((child, id));
                    }
                }
            }
        }
        Ok(found)
    }
}

/// The stamp of each unique index: the latest register stamp of its columns.
fn stamps(node: &Node, record: &Materialized) -> Vec<Option<Stamp>> {
    node.uniques
        .iter()
        .map(|unique| {
            unique
                .fields
                .iter()
                .filter_map(|field| record.stamps.get(field))
                .max()
                .or(record.created.as_ref())
                .cloned()
        })
        .collect()
}

impl<A: RowApplication> Inner<A> {
    fn constraints(&self) -> Option<Arc<Typed>> {
        self.workspace
            .typed
            .clone()
            .filter(|typed| typed.constraints.is_some())
    }
    /// The candidate of `node` after a stored row of its scope changed,
    /// inside the caller's transaction. Resolution waits for `resolve`.
    pub(super) async fn follow(
        &self,
        typed: &Typed,
        node: usize,
        scope: ResourceScope,
        new: &Materialized,
    ) -> Result<(), Error> {
        let constraints = typed
            .constraints
            .as_ref()
            .ok_or_else(|| corrupt("constraints"))?;
        let spec = &constraints.nodes[node];
        let table = &typed.tables[spec.table];
        let key = new.key.ok_or_else(|| corrupt("missing incarnation key"))?;
        let winner = self.winner(scope, key).await?;
        let mut pass = Pass::new(self, constraints);
        let values = |record: &Materialized| {
            typed
                .rows
                .values(&table.table, &record.fields)
                .map_err(Error::Corrupt)
        };
        let slot_values = match (&winner, new.deleted, new.identity) {
            (Some(winner), _, _) => Some(values(winner)?),
            (None, false, _) => Some(values(new)?),
            (None, true, Some(id)) => pass
                .get(node, Identity { scope, id })
                .await?
                .map(|held| held.values),
            (None, true, None) => None,
        };
        let Some(slot_values) = slot_values else {
            return Ok(());
        };
        let key_values: Vec<SqlValue> = spec.key.iter().map(|i| slot_values[*i].clone()).collect();
        let previous = pass
            .query(node, &spec.select_key.clone(), key_values.clone())
            .await?
            .pop();
        let mut seeds = std::mem::take(&mut *self.settling.lock().map_err(|_| corrupt("seeds"))?);
        if let Some(previous) = &previous {
            seeds.queued.extend(pass.dependents(node, previous).await?);
            // The rows it referenced may lose their last child.
            seeds
                .support
                .extend(spec.support_seeds(node, constraints, &previous.values));
        }
        seeds
            .support
            .extend(spec.support_seeds(node, constraints, &slot_values));
        match &winner {
            Some(winner) => {
                let candidate = Candidate {
                    scope,
                    live: previous
                        .as_ref()
                        .filter(|held| held.id(spec).map(|id| id.id) == winner.identity)
                        .is_some_and(|held| held.live),
                    values: values(winner)?,
                    stamps: stamps(spec, winner),
                };
                let mut params = candidate.values.clone();
                params.push(SqlValue::Integer(i64::from(candidate.live)));
                params.push(SqlValue::Blob(encode(&candidate.stamps)?));
                self.conn
                    .execute(spec.upsert.clone(), bindings(params))
                    .await?;
                seeds.forced.extend(candidate.id(spec).map(|id| (node, id)));
            }
            None => {
                self.conn
                    .execute(spec.delete_key.clone(), bindings(key_values.clone()))
                    .await?;
            }
        }
        let row = typed
            .rows
            .row_id(typed.domain.domain, &table.table, &slot_values)
            .map_err(Error::Corrupt)?;
        seeds.slots.insert((node, scope, row), key_values);
        #[cfg(any(test, feature = "qualification"))]
        seeds.dependents.extend(pass.dependents);
        *self.settling.lock().map_err(|_| corrupt("seeds"))? = seeds;
        Ok(())
    }
    /// Resolves what the collected changes affect and writes the typed rows
    /// that differ. In a SQL commit `written` names the slots the transaction
    /// wrote itself: they must already hold their resolved rows.
    pub(super) async fn resolve(
        &self,
        written: Option<&BTreeSet<(ResourceScope, Commitment)>>,
    ) -> Result<(), Error> {
        let seeds = std::mem::take(&mut *self.settling.lock().map_err(|_| corrupt("seeds"))?);
        let Some(typed) = self.constraints() else {
            return Ok(());
        };
        if seeds.is_empty() {
            return Ok(());
        }
        let constraints = typed
            .constraints
            .as_ref()
            .ok_or_else(|| corrupt("constraints"))?;
        let mut pass = Pass::new(self, constraints);
        #[cfg(any(test, feature = "qualification"))]
        let inputs: BTreeSet<_> = seeds.slots.keys().copied().collect();
        #[cfg(any(test, feature = "qualification"))]
        {
            pass.dependents = seeds.dependents;
        }
        let mut support = seeds.support;
        let mut desired: DesiredRows = seeds
            .slots
            .into_iter()
            .map(|(slot, key)| (slot, (key, None)))
            .collect();
        let mut lives = Vec::new();
        let mut queue: Vec<((usize, Identity), bool)> = seeds
            .forced
            .iter()
            .map(|row| (*row, true))
            .chain(seeds.queued.iter().map(|row| (*row, false)))
            .collect();
        let mut evaluated = BTreeSet::new();
        while let Some(((node, id), forced)) = queue.pop() {
            if !evaluated.insert((node, id)) {
                continue;
            }
            let Some(candidate) = pass.get(node, id).await? else {
                continue;
            };
            let spec = &constraints.nodes[node];
            let table = &typed.tables[spec.table];
            let image = pass.image(node, id).await?;
            let anchored = pass.anchored(node, id).await?;
            let key: Vec<SqlValue> = spec
                .key
                .iter()
                .map(|i| candidate.values[*i].clone())
                .collect();
            let stored = rows_with(&self.conn, &table.select, key.clone())
                .await?
                .pop();
            if anchored != candidate.live {
                lives.push((node, id, anchored));
            }
            if forced || anchored != candidate.live || stored != image {
                for row in pass.dependents(node, &candidate).await? {
                    queue.push((row, false));
                }
            }
            let row = typed
                .rows
                .row_id(typed.domain.domain, &table.table, &candidate.values)
                .map_err(Error::Corrupt)?;
            desired.insert((node, id.scope, row), (key, image));
        }
        // Rows leave before others take their values, so the real unique
        // index holds after every statement; references are checked at commit.
        let mut changes = Vec::new();
        for ((node, scope, row), (key, image)) in desired {
            let spec = &constraints.nodes[node];
            let table = &typed.tables[spec.table];
            self.index_seal_slot(&typed, scope, row, image.as_deref())
                .await?;
            let stored = rows_with(&self.conn, &table.select, key.clone())
                .await?
                .pop();
            // A support row stays until `settle_support` decides; a real row replaces it.
            let stored = stored.filter(|values| !spec.is_support(values));
            // Also when unchanged: a SQL commit wrote its rows itself.
            for values in stored.iter().chain(image.iter()) {
                support.extend(spec.support_seeds(node, constraints, values));
            }
            if stored == image {
                continue;
            }
            if written.is_some_and(|written| written.contains(&(scope, row))) {
                return Err(conflict(format!(
                    "a row of {} conflicts with a merged row it has not seen",
                    table.table.sql_name
                )));
            }
            // Only child visibility changes outside the input slots qualify for
            // the dependent fan-out exception; unique peers are not counted.
            #[cfg(any(test, feature = "qualification"))]
            if !inputs.contains(&(node, scope, row))
                && stored.iter().chain(image.iter()).any(|values| {
                    matches!(values.get(spec.id), Some(SqlValue::Integer(id))
                        if pass.dependents.contains(&(node, Identity { scope, id: *id })))
                })
            {
                self.flusher
                    .measurements
                    .work(super::lock::WorkUnit::DependentRow, 1);
            }
            self.touch(&table.table.sql_name);
            let held = rows_with(&self.conn, &table.select, key.clone())
                .await?
                .pop();
            if held.is_some() && (stored.is_some() || image.is_some()) {
                self.conn
                    .execute(table.delete.clone(), bindings(key))
                    .await?;
            }
            if let Some(image) = image {
                changes.push((spec.table, image));
            }
        }
        for (table, image) in changes {
            self.conn
                .execute(typed.tables[table].upsert.clone(), bindings(image))
                .await?;
        }
        for (node, id, live) in lives {
            self.conn
                .execute(
                    constraints.nodes[node].set_live.clone(),
                    bindings(
                        std::iter::once(SqlValue::Integer(i64::from(live)))
                            .chain(pass.identity_params(node, id)?)
                            .collect(),
                    ),
                )
                .await?;
        }
        Box::pin(self.settle_support(&typed, constraints, support)).await
    }
    /// Makes each seeded table `id` hold the support row it needs: a puppet
    /// for an alive row of an existence log that a typed row references and
    /// no real row stands for, the virtual row (`id = 0`) while a support
    /// row's `NOT NULL` reference names the table. Never replicated.
    async fn settle_support(
        &self,
        typed: &Typed,
        constraints: &Constraints,
        mut queue: BTreeSet<(usize, Identity)>,
    ) -> Result<(), Error> {
        let mut pass = Pass::new(self, constraints);
        while let Some((node, id)) = queue.pop_first() {
            let spec = &constraints.nodes[node];
            let Some(support) = &spec.support else {
                continue;
            };
            let kind = rows_with(&self.conn, &support.kind, pass.identity_params(node, id)?)
                .await?
                .pop();
            let real = kind.as_deref() == Some(&[SqlValue::Integer(0)]);
            let needed = if real {
                false
            } else if id.id == 0 {
                let mut needed = false;
                for (child, reference) in &spec.children {
                    let child = &constraints.nodes[*child];
                    if child.references[*reference].required
                        && let Some(owner) = &child.support
                        && !rows_with(
                            &self.conn,
                            "SELECT 1 FROM secure_support WHERE tab=? AND scope=? LIMIT 1",
                            vec![
                                SqlValue::Text(owner.name.clone()),
                                SqlValue::Blob(encode(&child.scope)?),
                            ],
                        )
                        .await?
                        .is_empty()
                    {
                        needed = true;
                        break;
                    }
                }
                needed
            } else {
                spec.logged
                    && pass.alive(node, id).await?
                    && self.referenced(typed, &mut pass, node, id).await?
            };
            let had = !rows_with(
                &self.conn,
                "SELECT 1 FROM secure_support WHERE tab=? AND scope=? AND id=?",
                vec![
                    SqlValue::Text(support.name.clone()),
                    SqlValue::Blob(encode(&id.scope)?),
                    SqlValue::Integer(id.id),
                ],
            )
            .await?
            .is_empty();
            match (needed, kind.is_some() && !real) {
                (true, false) => {
                    let mut values = support.values.clone();
                    for (column, value) in spec
                        .partition
                        .iter()
                        .zip(pass.partition_params(node, id.scope)?)
                    {
                        values[*column] = value;
                    }
                    values[support.id] = SqlValue::Integer(id.id);
                    values[support.row_kind] = SqlValue::Integer(if id.id == 0 { 2 } else { 1 });
                    self.conn
                        .execute(support.insert.clone(), bindings(values))
                        .await?;
                    self.touch(&support.name);
                }
                (false, true) => {
                    self.conn
                        .execute(
                            support.delete.clone(),
                            bindings(pass.identity_params(node, id)?),
                        )
                        .await?;
                    self.touch(&support.name);
                }
                _ => {}
            }
            if needed != had {
                let sql = if needed {
                    "INSERT INTO secure_support VALUES(?,?,?)"
                } else {
                    "DELETE FROM secure_support WHERE tab=? AND scope=? AND id=?"
                };
                self.conn
                    .execute(
                        sql,
                        bindings(vec![
                            SqlValue::Text(support.name.clone()),
                            SqlValue::Blob(encode(&id.scope)?),
                            SqlValue::Integer(id.id),
                        ]),
                    )
                    .await?;
                // The virtual rows its own references name follow it.
                for reference in spec.references.iter().filter(|r| r.required) {
                    queue.insert((
                        reference.parent,
                        Identity::unpartitioned(&constraints.nodes[reference.parent], 0),
                    ));
                }
            }
        }
        Ok(())
    }
    /// Whether a typed row of a child table references `id` of `node`.
    async fn referenced(
        &self,
        typed: &Typed,
        pass: &mut Pass<'_, A>,
        node: usize,
        id: Identity,
    ) -> Result<bool, Error> {
        for (child, reference) in pass.node(node).children.clone() {
            let spec = pass.node(child);
            let rule = &spec.references[reference];
            let column = rule.columns[0];
            let table = &typed.tables[spec.table];
            for candidate in pass
                .query(child, &rule.select.clone(), vec![SqlValue::Integer(id.id)])
                .await?
            {
                let key: Vec<SqlValue> = spec
                    .key
                    .iter()
                    .map(|i| candidate.values[*i].clone())
                    .collect();
                if rows_with(&self.conn, &table.select, key)
                    .await?
                    .first()
                    .is_some_and(|row| row[column] == SqlValue::Integer(id.id))
                {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
    /// After an existence event of `id` in `scope`: its own candidate, the
    /// candidates that reference it and its support row settle next.
    pub(super) async fn seed_existence(&self, scope: ResourceScope, id: i64) -> Result<(), Error> {
        let Some(typed) = self.constraints() else {
            return Ok(());
        };
        let constraints = typed
            .constraints
            .as_ref()
            .ok_or_else(|| corrupt("constraints"))?;
        let node = constraints
            .node(scope)
            .ok_or_else(|| corrupt("missing existence constraint scope"))?;
        let id = Identity { scope, id };
        let mut pass = Pass::new(self, constraints);
        let mut seeds = std::mem::take(&mut *self.settling.lock().map_err(|_| corrupt("seeds"))?);
        if pass.get(node, id).await?.is_some() {
            seeds.forced.insert((node, id));
        }
        for (child, reference) in constraints.nodes[node].children.clone() {
            let select = constraints.nodes[child].references[reference]
                .select
                .clone();
            for row in pass
                .query(child, &select, vec![SqlValue::Integer(id.id)])
                .await?
            {
                seeds
                    .queued
                    .extend(row.id(&constraints.nodes[child]).map(|id| (child, id)));
            }
        }
        seeds.support.insert((node, id));
        *self.settling.lock().map_err(|_| corrupt("seeds"))? = seeds;
        Ok(())
    }
    /// Rebuilds the selected partition, or the unpartitioned dependency graph.
    /// With no target, rebuilds all scopes; `without` is being evicted.
    pub(super) async fn rebuild_constraints(
        &self,
        target: Option<ResourceScope>,
        without: Option<ResourceScope>,
    ) -> Result<(), Error> {
        let Some(typed) = self.constraints() else {
            return Ok(());
        };
        let tables: Vec<String> = typed
            .constraints
            .as_ref()
            .ok_or_else(|| corrupt("constraints"))?
            .nodes
            .iter()
            .map(|node| typed.tables[node.table].table.sql_name.clone())
            .collect();
        self.in_bulk(&tables, self.rebuild_constraints_now(target, without))
            .await
    }
    async fn rebuild_constraints_now(
        &self,
        target: Option<ResourceScope>,
        without: Option<ResourceScope>,
    ) -> Result<(), Error> {
        let Some(typed) = self.constraints() else {
            return Ok(());
        };
        let constraints = typed
            .constraints
            .as_ref()
            .ok_or_else(|| corrupt("constraints"))?;
        let partition_only = target
            .and_then(|scope| constraints.node(scope))
            .is_some_and(|node| !constraints.nodes[node].partition.is_empty());
        let address = |scope: ResourceScope| -> Result<Option<rocky_core::GroupAddress>, Error> {
            let Some(table) = typed.by_scope(scope) else {
                return Ok(None);
            };
            let logical = typed
                .rows
                .schema()
                .table(&table.table.sql_name)
                .ok_or_else(|| corrupt("constraint table"))?;
            if logical.partition_by.is_empty() {
                return Ok(None);
            }
            let tuple = self
                .partitions
                .get(&scope)
                .ok_or_else(|| corrupt("constraint partition"))?;
            let values = logical
                .partition_by
                .iter()
                .map(|name| {
                    let column = logical
                        .column(name)
                        .ok_or_else(|| corrupt("partition column"))?;
                    tuple
                        .values()
                        .get(&typed.rows.schema().field_id(logical, column))
                        .ok_or_else(|| corrupt("partition value"))?
                        .value()
                        .map_err(|error| corrupt(error.to_string()))
                })
                .collect::<Result<Vec<_>, Error>>()?;
            typed
                .rows
                .schema()
                .group_address(scope.domain, &logical.sql_name, &values)
                .map(Some)
                .map_err(|error| corrupt(error.to_string()))
        };
        let target_address = target.map(address).transpose()?.flatten();
        let mut connected = BTreeSet::new();
        if let Some(node) = target.and_then(|scope| constraints.node(scope)) {
            let mut queue = vec![node];
            while let Some(node) = queue.pop() {
                if connected.insert(node) {
                    queue.extend(
                        constraints.nodes[node]
                            .references
                            .iter()
                            .map(|reference| reference.parent),
                    );
                    queue.extend(
                        constraints.nodes[node]
                            .children
                            .iter()
                            .map(|(child, _)| *child),
                    );
                }
            }
        }
        let mut scopes = Vec::new();
        for (node, spec) in constraints.nodes.iter().enumerate() {
            if target.is_some() && !partition_only && !connected.contains(&node) {
                continue;
            }
            if !spec.partition.is_empty() {
                for scope in self.partitions.keys().copied() {
                    if scope.domain == spec.scope.domain
                        && scope.table == spec.scope.table
                        && (!partition_only
                            || target == Some(scope)
                            || target_address.is_some() && address(scope)? == target_address)
                    {
                        scopes.push((node, scope));
                    }
                }
            } else if !partition_only {
                scopes.push((node, spec.scope));
            }
        }
        let selected: BTreeSet<_> = scopes.iter().copied().collect();
        {
            let mut seeds = self.settling.lock().map_err(|_| corrupt("seeds"))?;
            seeds
                .forced
                .retain(|(node, id)| !selected.contains(&(*node, id.scope)));
            seeds
                .queued
                .retain(|(node, id)| !selected.contains(&(*node, id.scope)));
            seeds
                .support
                .retain(|(node, id)| !selected.contains(&(*node, id.scope)));
            seeds
                .slots
                .retain(|(node, scope, _), _| !selected.contains(&(*node, *scope)));
        }
        let mut pass = Pass::new(self, constraints);
        for &(node, scope) in &scopes {
            let spec = &constraints.nodes[node];
            let table = &typed.tables[spec.table];
            let params = pass.partition_params(node, scope)?;
            let filter = if spec.partition.is_empty() {
                String::new()
            } else {
                format!(" WHERE {}", spec.partition_filter)
            };
            self.touch(&table.table.sql_name);
            self.conn
                .execute(
                    format!("DELETE FROM {}{filter}", quoted(&table.table.sql_name)),
                    bindings(params.clone()),
                )
                .await?;
            self.conn
                .execute(format!("{}{filter}", spec.clear), bindings(params))
                .await?;
            self.conn
                .execute(
                    "DELETE FROM secure_support WHERE tab=? AND scope=?",
                    bindings(vec![
                        SqlValue::Text(table.table.sql_name.clone()),
                        SqlValue::Blob(encode(&scope)?),
                    ]),
                )
                .await?;
            if Some(scope) == without || !self.holds(scope).await? {
                continue;
            }
            for record in typed::winners(self, scope).await? {
                let mut params = typed
                    .rows
                    .values(&table.table, &record.fields)
                    .map_err(Error::Corrupt)?;
                params.push(SqlValue::Integer(0));
                params.push(SqlValue::Blob(encode(&stamps(spec, &record))?));
                self.conn
                    .execute(spec.upsert.clone(), bindings(params))
                    .await?;
            }
        }
        for &(node, scope) in &scopes {
            let spec = &constraints.nodes[node];
            let table = &typed.tables[spec.table];
            let sql = if spec.partition.is_empty() {
                spec.select_all.clone()
            } else {
                format!("{} WHERE {}", spec.select_all, spec.partition_filter)
            };
            for candidate in pass
                .query(node, &sql, pass.partition_params(node, scope)?)
                .await?
            {
                let id = candidate
                    .id(spec)
                    .ok_or_else(|| corrupt("candidate identity"))?;
                let image = pass.image(node, id).await?;
                let row = typed
                    .rows
                    .row_id(typed.domain.domain, &table.table, &candidate.values)
                    .map_err(Error::Corrupt)?;
                self.index_seal_slot(&typed, scope, row, image.as_deref())
                    .await?;
                if let Some(image) = image {
                    self.conn
                        .execute(table.upsert.clone(), bindings(image))
                        .await?;
                }
                if pass.anchored(node, id).await? {
                    self.conn
                        .execute(
                            spec.set_live.clone(),
                            bindings(
                                std::iter::once(SqlValue::Integer(1))
                                    .chain(pass.identity_params(node, id)?)
                                    .collect(),
                            ),
                        )
                        .await?;
                }
            }
        }
        // Support rows follow from the real rows, as after every change.
        let mut support = BTreeSet::new();
        for &(node, scope) in &scopes {
            let spec = &constraints.nodes[node];
            let name = &typed.tables[spec.table].table.sql_name;
            let table = quoted(name);
            let logical = typed
                .rows
                .schema()
                .table(name)
                .ok_or_else(|| corrupt("constrained table"))?;
            for reference in &spec.references {
                if !constraints.nodes[reference.parent].logged {
                    continue;
                }
                let column = quoted(&logical.columns[reference.columns[0]].sql_name);
                for row in rows_with(
                    &self.conn,
                    &format!(
                        "SELECT DISTINCT {column} FROM {table} WHERE {column} IS NOT NULL{}",
                        if spec.partition.is_empty() {
                            String::new()
                        } else {
                            format!(" AND {}", spec.partition_filter)
                        }
                    ),
                    pass.partition_params(node, scope)?,
                )
                .await?
                {
                    if let [SqlValue::Integer(id)] = row.as_slice() {
                        support.insert((
                            reference.parent,
                            Identity::unpartitioned(&constraints.nodes[reference.parent], *id),
                        ));
                    }
                }
            }
        }
        Box::pin(self.settle_support(&typed, constraints, support)).await
    }
    /// The clock a local write of `fields` must pass to win on this device:
    /// the creation of the incarnations at its key and the stamps of rows
    /// sharing one of its unique values.
    pub(super) async fn constraint_clock(&self, change: &RowChange) -> Result<u64, Error> {
        let Some(typed) = self.constraints() else {
            return Ok(0);
        };
        let constraints = typed
            .constraints
            .as_ref()
            .ok_or_else(|| corrupt("constraints"))?;
        let (Some(node), Some(after)) = (constraints.node(change.scope), &change.after) else {
            return Ok(0);
        };
        let spec = &constraints.nodes[node];
        let table = &typed.tables[spec.table];
        let values = typed
            .rows
            .values(&table.table, after)
            .map_err(Error::Corrupt)?;
        let mut clock = 0;
        if let Some(winner) = self.winner(change.scope, change.row).await? {
            clock = clock.max(winner.created.map_or(0, |stamp| stamp.clock));
        }
        let mut pass = Pass::new(self, constraints);
        let candidate = Candidate {
            scope: change.scope,
            values,
            live: false,
            stamps: Vec::new(),
        };
        for unique in &spec.uniques {
            if let Some(tuple) = candidate.tuple(&unique.columns) {
                for member in pass.query(node, &unique.select, tuple).await? {
                    clock = member
                        .stamps
                        .iter()
                        .flatten()
                        .map(|stamp| stamp.clock)
                        .fold(clock, u64::max);
                }
            }
        }
        Ok(clock)
    }
    /// The merged image a typed row stands for: values a policy shows as
    /// NULL read back from the stored row, so a commit does not publish them.
    /// Returns the substituted columns.
    pub(super) async fn merged_image(
        &self,
        scope: ResourceScope,
        before: &mut [SqlValue],
    ) -> Result<Vec<usize>, Error> {
        let Some(typed) = self.constraints() else {
            return Ok(Vec::new());
        };
        let constraints = typed
            .constraints
            .as_ref()
            .ok_or_else(|| corrupt("constraints"))?;
        let Some(node) = constraints.node(scope) else {
            return Ok(Vec::new());
        };
        let spec = &constraints.nodes[node];
        let Some(SqlValue::Integer(id)) = before.get(spec.id).cloned() else {
            return Ok(Vec::new());
        };
        let mut pass = Pass::new(self, constraints);
        let Some(candidate) = pass.get(node, Identity { scope, id }).await? else {
            return Ok(Vec::new());
        };
        let mut substituted = Vec::new();
        for (index, value) in before.iter_mut().enumerate() {
            if *value == SqlValue::Null && candidate.values[index] != SqlValue::Null {
                *value = candidate.values[index].clone();
                substituted.push(index);
            }
        }
        Ok(substituted)
    }
    /// Refuses a reference of the transaction's rows to a parent that is not
    /// in the typed table, as SQLite's deferred check would at commit.
    pub(super) async fn check_references(
        &self,
        scope: ResourceScope,
        after: &[SqlValue],
    ) -> Result<(), Error> {
        let Some(typed) = self.constraints() else {
            return Ok(());
        };
        let constraints = typed
            .constraints
            .as_ref()
            .ok_or_else(|| corrupt("constraints"))?;
        let Some(node) = constraints.node(scope) else {
            return Ok(());
        };
        let mut pass = Pass::new(self, constraints);
        let candidate = Candidate {
            scope,
            values: after.to_vec(),
            live: false,
            stamps: Vec::new(),
        };
        for reference in &constraints.nodes[node].references {
            let Some(identity) = pass.parent(&candidate, reference)? else {
                continue;
            };
            let spec = &constraints.nodes[reference.parent];
            let parent = &typed.tables[spec.table];
            let params = reference
                .columns
                .iter()
                .map(|column| after[*column].clone())
                .collect::<Vec<_>>();
            let logical = typed
                .rows
                .schema()
                .table(&parent.table.sql_name)
                .ok_or_else(|| corrupt("parent table"))?;
            let filter = reference
                .parent_columns
                .iter()
                .map(|column| format!("{}=?", quoted(&logical.columns[*column].sql_name)))
                .collect::<Vec<_>>()
                .join(" AND ");
            // A held row or an alive puppet, never the virtual row; an alive
            // row of an existence log gets its puppet as the commit settles.
            let kinds = if spec.logged { "(0,1)" } else { "(0)" };
            let mut found = !rows_with(
                &self.conn,
                &format!(
                    "SELECT 1 FROM {} WHERE {filter} AND \"row_kind\" IN {kinds}",
                    quoted(&parent.table.sql_name)
                ),
                params,
            )
            .await?
            .is_empty();
            if !found && spec.logged {
                found = pass.alive(reference.parent, identity).await?;
            }
            if !found {
                return Err(Error::Sql(crate::sql::SqlError::Statement {
                    code: 787,
                    message: "FOREIGN KEY constraint failed".into(),
                }));
            }
        }
        Ok(())
    }
}

async fn rows_with(
    conn: &crate::scoped_connection::ScopedConnection,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<Vec<Vec<SqlValue>>, Error> {
    collect_rows(conn.query(sql.to_string(), bindings(params)).await?).await
}
