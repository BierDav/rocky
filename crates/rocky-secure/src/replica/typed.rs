//! Typed tables of a secure replica: the generated SQL storage that
//! authorized secure rows project into, in the same transaction as every
//! row change. See `docs/runtime/secure-sql.md`.
use super::*;
use crate::{capture::Plan, scoped_connection::Names, storage};
use rocky_core::{LogicalSchema, Scope, TableClass, TypedRows, TypedTable};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_projection_access(id INTEGER PRIMARY KEY CHECK(id=1),tables BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_typed(id INTEGER PRIMARY KEY CHECK(id=1),descriptor BLOB NOT NULL,domain BLOB NOT NULL,readers BLOB NOT NULL)",
];

/// One typed table and the resource scope its rows belong to.
#[derive(Clone)]
pub(super) struct Projected {
    pub(super) table: TypedTable,
    pub(super) scope: ResourceScope,
    pub(super) select: String,
    pub(super) upsert: String,
    pub(super) delete: String,
}
/// The installed typed tables of a replica.
pub(super) struct Typed {
    pub(super) rows: TypedRows,
    pub(super) domain: Scope,
    pub(super) tables: Vec<Projected>,
    /// Full signed declarations, including tables with no physical projection.
    pub(super) declarations: Vec<Projected>,
    pub(super) names: Names,
    /// Whether these are the shadow tables of a projection plan.
    pub(super) shadowed: bool,
    /// Tables with unique indexes or foreign keys; never on shadows.
    pub(super) constraints: Option<super::constraints::Constraints>,
}
pub(super) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
/// The stored name of a typed table, or of its shadow during a plan.
pub(super) fn physical(sql_name: &str, shadow: bool) -> String {
    if shadow {
        format!("{SHADOW}_{sql_name}")
    } else {
        sql_name.to_string()
    }
}
/// The namespace of shadow tables and their indexes.
pub(super) const SHADOW: &str = "secure_shadow";
impl Typed {
    pub(super) fn new(
        schema: LogicalSchema,
        domain: Scope,
        readers: &BTreeMap<String, Commitment>,
    ) -> Result<Self, Error> {
        Self::build(schema, domain, readers, false)
    }
    /// The shadow tables a projection plan fills: the same columns under
    /// `secure_shadow_` names, which applications cannot use.
    pub(super) fn shadow(
        schema: LogicalSchema,
        domain: Scope,
        readers: &BTreeMap<String, Commitment>,
    ) -> Result<Self, Error> {
        Self::build(schema, domain, readers, true)
    }
    fn build(
        schema: LogicalSchema,
        domain: Scope,
        readers: &BTreeMap<String, Commitment>,
        shadow: bool,
    ) -> Result<Self, Error> {
        let incompatible = |reason: String| Error::Incompatible(reason);
        // Rows written before a transition added a field lack it; the
        // application admitted them, so they project with NULL or, for a
        // required field, its default.
        let rows = TypedRows::new(schema.clone(), vec![domain.domain])
            .and_then(|rows| rows.at_generation(1, rocky_core::fillable(&schema)))
            .map_err(|error| incompatible(error.to_string()))?;
        let mut tables = Vec::new();
        let typed: Vec<_> = rows.tables().collect();
        for table in typed.iter() {
            let logical = schema
                .table(&table.sql_name)
                .ok_or_else(|| incompatible(format!("{} is not in the schema", table.sql_name)))?;
            let encryption = readers
                .get(&table.sql_name)
                .ok_or_else(|| incompatible(format!("{} has no reader scope", table.sql_name)))?;
            let columns: Vec<String> = logical
                .columns
                .iter()
                .map(|column| quoted(&column.sql_name))
                .collect();
            let key: Vec<String> = table
                .key
                .iter()
                .map(|index| columns[*index].clone())
                .collect();
            let by_key = key
                .iter()
                .map(|column| format!("{column}=?"))
                .collect::<Vec<_>>()
                .join(" AND ");
            let registers: Vec<String> = columns
                .iter()
                .filter(|column| !key.contains(column))
                .map(|column| format!("{column}=excluded.{column}"))
                .collect();
            let name = quoted(&physical(&table.sql_name, shadow));
            let conflict = if registers.is_empty() {
                "DO NOTHING".to_string()
            } else {
                format!("DO UPDATE SET {}", registers.join(","))
            };
            tables.push(Projected {
                scope: ResourceScope {
                    domain,
                    encryption: *encryption,
                    table: table.id,
                },
                select: format!("SELECT {} FROM {name} WHERE {by_key}", columns.join(",")),
                upsert: format!(
                    "INSERT INTO {name}({}) VALUES({}) ON CONFLICT({}) {conflict}",
                    columns.join(","),
                    vec!["?"; columns.len()].join(","),
                    key.join(","),
                ),
                delete: format!("DELETE FROM {name} WHERE {by_key}"),
                table: (*table).clone(),
            });
        }
        if readers.len() != typed.len() {
            return Err(incompatible("a reader scope names no typed table".into()));
        }
        let statements = statements(&schema);
        let constraints = super::constraints::Constraints::new(&schema, &tables)?;
        if shadow && constraints.is_some() {
            return Err(incompatible(
                "tables with unique indexes or foreign keys change by export and reinstall".into(),
            ));
        }
        Ok(Self {
            names: Names::new(None, &statements),
            rows,
            domain,
            declarations: tables.clone(),
            tables,
            shadowed: shadow,
            constraints,
        })
    }
    fn restrict(mut self, allowed: &BTreeSet<String>) -> Result<Self, Error> {
        self.tables
            .retain(|table| allowed.contains(&table.table.sql_name));
        self.constraints = super::constraints::Constraints::new(self.rows.schema(), &self.tables)?;
        Ok(self)
    }
    pub(super) fn statements(&self) -> Vec<String> {
        self.rows
            .schema()
            .tables
            .iter()
            .filter(|table| {
                table.class == TableClass::LocalOnly || self.by_name(&table.sql_name).is_some()
            })
            .flat_map(|table| {
                std::iter::once(table.create_sql.clone())
                    .chain(table.indexes.iter().map(|index| index.create_sql.clone()))
            })
            .collect()
    }
    pub(super) fn partitioned(&self, table: &Projected) -> bool {
        self.rows
            .schema()
            .table(&table.table.sql_name)
            .is_some_and(|logical| !logical.partition_by.is_empty())
    }
    pub(super) fn row_scope(
        &self,
        table: &Projected,
        values: &[SqlValue],
    ) -> Result<ResourceScope, Error> {
        match self
            .rows
            .partition(&table.table, values)
            .map_err(|error| denied(&error))?
        {
            Some(tuple) => tuple
                .scope(self.domain, table.table.id)
                .map_err(|error| denied(&error.to_string())),
            None => Ok(table.scope),
        }
    }
    pub(super) fn by_scope(&self, scope: ResourceScope) -> Option<&Projected> {
        self.tables.iter().find(|table| {
            table.scope == scope
                || (self.partitioned(table)
                    && table.scope.domain == scope.domain
                    && table.scope.table == scope.table)
        })
    }
    pub(super) fn by_name(&self, sql_name: &str) -> Option<&Projected> {
        self.tables
            .iter()
            .find(|table| table.table.sql_name.eq_ignore_ascii_case(sql_name))
    }
    pub(super) fn declared_by_scope(&self, scope: ResourceScope) -> Option<&Projected> {
        self.declarations.iter().find(|table| {
            table.scope == scope
                || (self.partitioned(table)
                    && table.scope.domain == scope.domain
                    && table.scope.table == scope.table)
        })
    }
    pub(super) fn declared_by_name(&self, sql_name: &str) -> Option<&Projected> {
        self.declarations
            .iter()
            .find(|table| table.table.sql_name.eq_ignore_ascii_case(sql_name))
    }
    /// The tables a session may name: every local table and the mergeable
    /// tables of `scopes`.
    pub(super) fn plan(&self, scopes: &BTreeSet<ResourceScope>) -> Result<Plan, Error> {
        let schema = self.rows.schema();
        let named = |table: &rocky_core::LogicalTable| match self.by_name(&table.sql_name) {
            Some(projected) => {
                scopes.contains(&projected.scope)
                    || (self.partitioned(projected)
                        && scopes.iter().any(|scope| {
                            scope.domain == projected.scope.domain
                                && scope.table == projected.scope.table
                        }))
            }
            None => table.class == TableClass::LocalOnly,
        };
        // A table with an existence log that a named table references is
        // nameable too; the statement check shows none of its support rows.
        let tables: Vec<&str> = schema
            .tables
            .iter()
            .filter(|table| {
                named(table)
                    || (table.existence_log
                        && schema.tables.iter().any(|child| {
                            named(child)
                                && child
                                    .foreign_keys
                                    .iter()
                                    .any(|key| key.table.eq_ignore_ascii_case(&table.sql_name))
                        }))
            })
            .map(|table| table.sql_name.as_str())
            .collect();
        Plan::bound(&[crate::capture::Part {
            schema,
            names: &self.names,
            tables: Some(tables),
        }])
    }
}
pub(super) fn statements(schema: &LogicalSchema) -> Vec<String> {
    schema
        .tables
        .iter()
        .flat_map(|table| {
            std::iter::once(table.create_sql.clone())
                .chain(table.indexes.iter().map(|index| index.create_sql.clone()))
        })
        .collect()
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Installs only tables covered by local read grants or partition selectors.
    /// The complete descriptor still has to match the signed manifest.
    pub async fn install_readable_tables(
        &self,
        schema: LogicalSchema,
        domain: Scope,
        readers: BTreeMap<String, Commitment>,
    ) -> Result<(), Error> {
        {
            let mut inner = self.inner.lock().await;
            inner.validate_typed(&schema, domain)?;
            let typed = Typed::new(schema.clone(), domain, &readers)?;
            let allowed = inner.readable_names(&typed)?;
            typed.restrict(&allowed)?;
            transaction!(inner, |inner| {
                let previous = inner.projection_access().await?;
                if previous.is_none() && inner.installed_typed().await?.is_some() {
                    return Err(Error::Incompatible("a complete projection cannot become a readable projection; reinstall the database".into()));
                }
                // Grants may widen, as a renewal adds a table granted like a
                // held one; the plan below projects it. They never narrow.
                if let Some(previous) = previous.as_ref()
                    && !previous.is_subset(&allowed)
                {
                    return Err(Error::Incompatible(
                        "projection grants changed; reinstall the database".into(),
                    ));
                }
                inner.conn.execute("INSERT INTO secure_projection_access VALUES(1,?) ON CONFLICT(id) DO UPDATE SET tables=excluded.tables", [encode(&allowed)?]).await?;
                Ok(())
            })?;
        }
        self.install_tables(schema, domain, readers).await
    }
    /// Installs typed tables for `schema` in `domain`; `readers` names the
    /// encryption scope of every mergeable table. The schema's manifest must
    /// be the replica's signed manifest. Existing rows project at once;
    /// reopening keeps the installation. Another schema of the same signed
    /// definition, such as new indexes, replaces it through a projection
    /// plan: shadow tables fill in bounded steps and switch in one verified
    /// transaction. Changes the definition does not allow are refused.
    pub async fn install_tables(
        &self,
        schema: LogicalSchema,
        domain: Scope,
        readers: BTreeMap<String, Commitment>,
    ) -> Result<(), Error> {
        schema
            .require_row_identity()
            .map_err(|error| Error::Incompatible(error.to_string()))?;
        let mut inner = self.inner.lock().await;
        inner.validate_typed(&schema, domain)?;
        let typed = inner
            .authorized_projection(Typed::new(schema.clone(), domain, &readers)?)
            .await?;
        let record = [
            schema
                .canonical()
                .map_err(|error| Error::Incompatible(error.to_string()))?,
            encode(&domain)?,
            encode(&readers)?,
        ];
        let planned = transaction!(inner, |inner| {
            let stored = rows(
                &inner.conn,
                "SELECT descriptor,domain,readers FROM secure_typed WHERE id=1",
            )
            .await?;
            let expected: Vec<SqlValue> = record.iter().cloned().map(SqlValue::Blob).collect();
            match stored.first() {
                // Grants that widened over the same schema project the
                // tables they add through a plan, as another schema does.
                Some(row) if *row == expected => {
                    if inner.projection_grew(&typed).await? {
                        inner.record_growth().await?;
                        return Ok(true);
                    }
                    return Ok(false);
                }
                // Another schema of the same definition: rebuild the tables
                // through a projection plan.
                Some(_) => {
                    inner.record_plan(&schema, domain, &readers).await?;
                    return Ok(true);
                }
                None => {}
            }
            for sql in typed.statements().iter().chain(
                typed
                    .constraints
                    .iter()
                    .flat_map(super::constraints::Constraints::statements),
            ) {
                inner.conn.execute(sql.clone(), ()).await?;
            }
            inner
                .conn
                .execute(
                    "INSERT INTO secure_typed VALUES(1,?,?,?)",
                    bindings(expected),
                )
                .await?;
            for table in &typed.tables {
                if typed
                    .constraints
                    .as_ref()
                    .and_then(|c| c.node(table.scope))
                    .is_none()
                {
                    reproject(&inner.conn, &typed, table).await?;
                }
            }
            Ok(false)
        })?;
        if planned {
            drop(inner);
            return self.run_plan().await;
        }
        verify_constraint_indexes(&inner.conn, &typed).await?;
        let constrained = typed.constraints.is_some();
        inner.workspace.typed = Some(Arc::new(typed));
        if constrained {
            transaction!(inner, |inner| Box::pin(
                inner.rebuild_constraints(None, None)
            )
            .await)?;
        }
        inner.start_events();
        Ok(())
    }
}

impl<A: RowApplication> Inner<A> {
    fn validate_typed(&self, schema: &LogicalSchema, domain: Scope) -> Result<(), Error> {
        schema
            .require_row_identity()
            .map_err(|error| Error::Incompatible(error.to_string()))?;
        let domains: Vec<_> = self
            .definition
            .domains
            .iter()
            .map(|rule| rule.domain)
            .collect();
        if domain.app != self.definition.app || !domains.contains(&domain.domain) {
            return Err(denied("typed tables need a domain of the signed manifest"));
        }
        // The generation is the installed application's; the tables must be
        // exactly the signed ones.
        let expected = schema.manifest_at(&domains, self.definition.schema);
        if expected.hash() != self.manifest {
            return Err(Error::Incompatible(
                "the schema's definition differs from the signed manifest".into(),
            ));
        }
        if schema
            .tables
            .iter()
            .any(|table| table.sql_name.to_ascii_lowercase().starts_with("secure_"))
        {
            return Err(Error::Incompatible("secure_ names are reserved".into()));
        }
        Ok(())
    }
    async fn projection_access(&self) -> Result<Option<BTreeSet<String>>, Error> {
        let mut rows = self
            .conn
            .query("SELECT tables FROM secure_projection_access WHERE id=1", ())
            .await?;
        rows.next()
            .await?
            .map(|row| decode(&row.get::<Vec<u8>>(0)?))
            .transpose()
    }
    pub(super) fn readable_names(&self, typed: &Typed) -> Result<BTreeSet<String>, Error> {
        let (grants, selectors) = self
            .config
            .credentials
            .verify_selectors(
                self.config.publisher,
                self.config.node.public_key(),
                &self.config.revocations,
                now()?,
            )
            .map_err(credential)?;
        let mut allowed: BTreeSet<String> = typed
            .tables
            .iter()
            .filter(|table| {
                grants
                    .iter()
                    .any(|grant| grant.scope == table.scope && grant.rights.contains(&Right::Read))
                    || selectors.iter().any(|grant| {
                        grant.domain == typed.domain
                            && grant.table == table.scope.table
                            && grant.rights.contains(&Right::Read)
                    })
            })
            .map(|table| table.table.sql_name.clone())
            .collect();
        loop {
            let before = allowed.len();
            for table in &typed.rows.schema().tables {
                if allowed.contains(&table.sql_name) {
                    for key in &table.foreign_keys {
                        if typed
                            .rows
                            .schema()
                            .table(&key.table)
                            .is_some_and(|parent| parent.existence_log)
                        {
                            allowed.insert(key.table.clone());
                        }
                    }
                }
            }
            if before == allowed.len() {
                break;
            }
        }
        Ok(allowed)
    }
    pub(super) async fn authorized_projection(&self, typed: Typed) -> Result<Typed, Error> {
        match self.projection_access().await? {
            Some(_) => {
                let allowed = self.readable_names(&typed)?;
                typed.restrict(&allowed)
            }
            None => Ok(typed),
        }
    }
    pub(super) async fn record_projection_access(&self, typed: &Typed) -> Result<(), Error> {
        if self.projection_access().await?.is_some() {
            let allowed: BTreeSet<String> = typed
                .tables
                .iter()
                .map(|table| table.table.sql_name.clone())
                .collect();
            self.conn
                .execute(
                    "UPDATE secure_projection_access SET tables=? WHERE id=1",
                    [encode(&allowed)?],
                )
                .await?;
        }
        Ok(())
    }
    /// Loads the installed typed tables at open.
    pub(super) async fn load_typed(&mut self) -> Result<(), Error> {
        let stored = rows(
            &self.conn,
            "SELECT descriptor,domain,readers FROM secure_typed WHERE id=1",
        )
        .await?;
        let Some(row) = stored.first() else {
            return Ok(());
        };
        let [
            SqlValue::Blob(descriptor),
            SqlValue::Blob(domain),
            SqlValue::Blob(readers),
        ] = row.as_slice()
        else {
            return Err(Error::Corrupt("typed table record".into()));
        };
        let schema = LogicalSchema::parse(descriptor)
            .map_err(|_| Error::Corrupt("typed table descriptor".into()))?;
        let typed = self
            .authorized_projection(Typed::new(schema, decode(domain)?, &decode(readers)?)?)
            .await?;
        verify_constraint_indexes(&self.conn, &typed).await?;
        self.workspace.typed = Some(Arc::new(typed));
        self.start_events();
        Ok(())
    }
    /// Follows one stored row into its typed table, inside the caller's
    /// transaction. An unchanged typed row is not written again, so rows a
    /// SQL session wrote fire no second trigger. In `strict` mode, during
    /// a SQL session's commit, a difference fails the commit instead.
    pub(super) async fn project(
        &self,
        scope: ResourceScope,
        old: Option<&Materialized>,
        new: &Materialized,
    ) -> Result<(), Error> {
        if let Some(shadow) = self.workspace.shadow.clone()
            && let Some(table) = shadow.by_scope(scope)
        {
            self.project_into(&shadow, table, scope, old, new, false)
                .await?;
        }
        let Some(typed) = self.workspace.typed.clone() else {
            return Ok(());
        };
        // Candidates follow every write; `resolve` settles them once the
        // frame or the SQL commit stored all of its rows.
        if let Some(node) = typed.constraints.as_ref().and_then(|c| c.node(scope)) {
            return Box::pin(self.follow(&typed, node, scope, new)).await;
        }
        if self.strict_projection {
            return Ok(());
        }
        let Some(table) = typed.by_scope(scope) else {
            return Ok(());
        };
        self.project_into(&typed, table, scope, old, new, self.strict_projection)
            .await
    }
    pub(super) async fn verify_sql_projection(&self, writes: &[RowWrite]) -> Result<(), Error> {
        if !self.strict_projection {
            return Ok(());
        }
        let typed = self
            .workspace
            .typed
            .clone()
            .ok_or_else(|| Error::Corrupt("missing SQL projection".into()))?;
        let written = writes
            .iter()
            .map(|write| (write.change.scope, write.change.row))
            .collect();
        Box::pin(self.resolve(Some(&written))).await?;
        let constraints = typed.constraints.as_ref();
        for write in writes {
            let change = &write.change;
            if constraints.and_then(|c| c.node(change.scope)).is_some() {
                continue;
            }
            let table = typed
                .by_scope(change.scope)
                .ok_or_else(|| Error::Corrupt("missing SQL table".into()))?;
            let record = self
                .row(change.scope, self.storage_row(change)?)
                .await?
                .ok_or_else(|| Error::Corrupt("missing SQL incarnation".into()))?;
            let old = change.before.as_ref().map(|fields| {
                let mut old = record.clone();
                old.fields = fields.clone();
                old.deleted = false;
                old
            });
            self.project_into(&typed, table, change.scope, old.as_ref(), &record, true)
                .await?;
        }
        Ok(())
    }
    /// Follows one stored row into one table, typed or shadow.
    async fn project_into(
        &self,
        typed: &Typed,
        table: &Projected,
        scope: ResourceScope,
        old: Option<&Materialized>,
        new: &Materialized,
        strict: bool,
    ) -> Result<(), Error> {
        let winner = if new.identity.is_some() {
            self.winner(
                scope,
                new.key
                    .ok_or_else(|| Error::Corrupt("missing incarnation key".into()))?,
            )
            .await?
        } else {
            Some(new.clone())
        };
        let live = |record: &Materialized| (!record.deleted).then(|| record.fields.clone());
        let corrupt = |reason: String| Error::Corrupt(reason);
        let current = match winner
            .as_ref()
            .and_then(live)
            .or_else(|| old.and_then(live))
            .or_else(|| live(new))
        {
            Some(fields) => typed.rows.values(&table.table, &fields).map_err(corrupt)?,
            None => return Ok(()),
        };
        let key = |values: &[SqlValue]| -> Vec<SqlValue> {
            table
                .table
                .key
                .iter()
                .filter_map(|index| values.get(*index).cloned())
                .collect()
        };
        let stored: Vec<Vec<SqlValue>> =
            rows_with(&self.conn, &table.select, key(&current)).await?;
        let wanted = winner.as_ref().and_then(live).map(|_| current.clone());
        if stored.first() == wanted.as_ref() || (stored.is_empty() && wanted.is_none()) {
            return Ok(());
        }
        if strict {
            return Err(Error::Sql(crate::sql::SqlError::Capture(format!(
                "the projection of {} differs from the transaction's rows",
                table.table.sql_name
            ))));
        }
        if !typed.shadowed {
            self.touch(&table.table.sql_name);
        }
        match wanted {
            Some(values) => {
                self.conn
                    .execute(table.upsert.clone(), bindings(values))
                    .await?
            }
            None => {
                self.conn
                    .execute(table.delete.clone(), bindings(key(&current)))
                    .await?
            }
        };
        Ok(())
    }
    /// Empties a scope's typed table, as its rows are evicted.
    pub(crate) async fn clear_typed(&self, scope: ResourceScope) -> Result<(), Error> {
        self.restart_plan(scope).await?;
        if self
            .workspace
            .typed
            .as_ref()
            .is_some_and(|typed| typed.declared_by_scope(scope).is_none())
        {
            return Err(Error::Corrupt("missing typed scope for eviction".into()));
        }
        if let Some(typed) = &self.workspace.typed
            && typed
                .constraints
                .as_ref()
                .and_then(|c| c.node(scope))
                .is_some()
        {
            // Children elsewhere lose their parents with it.
            return Box::pin(self.rebuild_constraints(Some(scope), Some(scope))).await;
        }
        if let Some(table) = self.workspace.typed.as_ref().and_then(|typed| typed.by_scope(scope)) {
            self.touch(&table.table.sql_name);
            reproject(
                &self.conn,
                self.workspace.typed
                    .as_ref()
                    .ok_or_else(|| denied("missing typed schema"))?,
                table,
            )
            .await?;
        }
        Ok(())
    }
    /// Rebuilds a scope's typed table from its stored rows, after a bulk
    /// change of `secure_rows`.
    pub(super) async fn reproject(&self, scope: ResourceScope) -> Result<(), Error> {
        self.index_row_keys().await?;
        self.reindex_move_scope(scope).await?;
        self.restart_plan(scope).await?;
        let Some(typed) = self.workspace.typed.clone() else {
            return Ok(());
        };
        if typed
            .constraints
            .as_ref()
            .and_then(|c| c.node(scope))
            .is_some()
        {
            return Box::pin(self.rebuild_constraints(Some(scope), None)).await;
        }
        match typed.by_scope(scope) {
            Some(table) => {
                self.touch(&table.table.sql_name);
                reproject(&self.conn, &typed, table).await
            }
            None if typed.declared_by_scope(scope).is_some() => Ok(()),
            None => Err(Error::Corrupt("missing typed scope for rebuild".into())),
        }
    }
}

async fn rows_with(
    conn: &crate::scoped_connection::ScopedConnection,
    sql: &str,
    params: Vec<SqlValue>,
) -> Result<Vec<Vec<SqlValue>>, Error> {
    collect_rows(conn.query(sql.to_string(), bindings(params)).await?).await
}

/// Replaces a typed table's rows by the live stored rows of its scope, or
/// of every partition scope of a partitioned table.
pub(super) async fn reproject(
    conn: &crate::scoped_connection::ScopedConnection,
    typed: &Typed,
    table: &Projected,
) -> Result<(), Error> {
    conn.execute(format!("DELETE FROM {}", quoted(&table.table.sql_name)), ())
        .await?;
    let records = if typed.partitioned(table) {
        stored_winners(conn, None, |scope| {
            typed
                .by_scope(scope)
                .is_some_and(|projected| projected.table.id == table.table.id)
        })
        .await?
    } else {
        winners(conn, table.scope).await?
    };
    for record in records {
        let values = typed
            .rows
            .values(&table.table, &record.fields)
            .map_err(Error::Corrupt)?;
        conn.execute(table.upsert.clone(), storage::bindings(values))
            .await?;
    }
    Ok(())
}
/// The live incarnation with the latest creation at each key of a scope.
pub(super) async fn winners(
    conn: &crate::scoped_connection::ScopedConnection,
    scope: ResourceScope,
) -> Result<Vec<Materialized>, Error> {
    stored_winners(conn, Some(scope), |_| true).await
}
/// The live winners of `scope`, or of every stored scope `keep` accepts.
async fn stored_winners(
    conn: &crate::scoped_connection::ScopedConnection,
    scope: Option<ResourceScope>,
    keep: impl Fn(ResourceScope) -> bool,
) -> Result<Vec<Materialized>, Error> {
    let mut after = (Vec::new(), Vec::new());
    let mut winners: BTreeMap<(ResourceScope, Commitment), Materialized> = BTreeMap::new();
    loop {
        let (sql, params) = match scope {
            Some(scope) => (
                "SELECT scope,row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT 256",
                vec![
                    SqlValue::Blob(encode(&scope)?),
                    SqlValue::Blob(after.1.clone()),
                ],
            ),
            None => (
                "SELECT scope,row,fields FROM secure_rows WHERE (scope,row)>(?,?) ORDER BY scope,row LIMIT 256",
                vec![
                    SqlValue::Blob(after.0.clone()),
                    SqlValue::Blob(after.1.clone()),
                ],
            ),
        };
        let chunk = collect_rows(conn.query(sql, bindings(params)).await?).await?;
        for record in &chunk {
            let [
                SqlValue::Blob(scope),
                SqlValue::Blob(row),
                SqlValue::Blob(bytes),
            ] = record.as_slice()
            else {
                return Err(Error::Corrupt("stored row".into()));
            };
            after = (scope.clone(), row.clone());
            let scope: ResourceScope = decode(scope)?;
            if !keep(scope) {
                continue;
            }
            let record: Materialized = decode(bytes)?;
            if record.deleted {
                continue;
            }
            let key = record.key.map(Ok).unwrap_or_else(|| {
                row.as_slice()
                    .try_into()
                    .map_err(|_| Error::Corrupt("row key".into()))
            })?;
            let key = (scope, key);
            if winners.get(&key).is_none_or(|held| {
                (record.created.as_ref(), record.identity) > (held.created.as_ref(), held.identity)
            }) {
                winners.insert(key, record);
            }
        }
        if chunk.len() < 256 {
            return Ok(winners.into_values().collect());
        }
    }
}

/// Refuse a missing or changed index before using persisted projections.
pub(super) async fn verify_constraint_indexes(
    conn: &crate::scoped_connection::ScopedConnection,
    typed: &Typed,
) -> Result<(), Error> {
    let Some(constraints) = &typed.constraints else {
        return Ok(());
    };
    let catalog = rows(conn, "SELECT sql FROM sqlite_schema WHERE sql IS NOT NULL").await?;
    let expected = typed
        .rows
        .schema()
        .tables
        .iter()
        .filter(|table| {
            table.class == TableClass::LocalOnly || typed.by_name(&table.sql_name).is_some()
        })
        .flat_map(|table| table.indexes.iter().map(|index| &index.create_sql))
        .chain(constraints.statements().filter(|sql| {
            sql.starts_with("CREATE ")
                && (sql.contains(" INDEX ") || sql.starts_with("CREATE TABLE "))
        }));
    for sql in expected
        .map(String::as_str)
        .chain(super::constraints::SCHEMA.iter().copied())
    {
        let canonical = sql.replace(" IF NOT EXISTS", "");
        if !catalog
            .iter()
            .any(|row| row == &[SqlValue::Text(canonical.clone())])
        {
            return Err(Error::Incompatible(format!(
                "missing or changed constraint index: {canonical}"
            )));
        }
    }
    Ok(())
}
