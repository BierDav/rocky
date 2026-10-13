//! Projection plans: a typed schema change rebuilds its tables as shadows
//! from the stored rows, in bounded transactions, and switches them in one
//! verified transaction. See `docs/runtime/generations.md`.
use super::typed::{SHADOW, Typed, physical, quoted, statements};
use super::*;
use crate::scoped_connection::Names;
use rocky_core::{LogicalSchema, SchemaChange, Scope, TableClass};

pub(super) const SCHEMA: &[&str] = &[
    // `position` is -1 before the shadow tables exist, then the mergeable
    // table being filled after `cursor`, then their count once all are.
    "CREATE TABLE IF NOT EXISTS secure_projection_plan(id INTEGER PRIMARY KEY CHECK(id=1),descriptor BLOB NOT NULL,domain BLOB NOT NULL,readers BLOB NOT NULL,position INTEGER NOT NULL,cursor BLOB NOT NULL)",
];
/// Stored rows copied into a shadow table per transaction.
pub const PLAN_ROWS: usize = 256;

/// Where a projection plan stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionPlan {
    /// The fingerprint of the schema being installed.
    pub schema: Commitment,
    /// Mergeable tables filled so far, of `tables`.
    pub filled: usize,
    pub tables: usize,
}
/// A step of a projection plan where tests interrupt it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PlanStage {
    Create,
    Fill,
    Switch,
}

struct Record {
    schema: LogicalSchema,
    domain: Scope,
    readers: BTreeMap<String, Commitment>,
    position: i64,
    cursor: Vec<u8>,
}

impl<A: RowApplication> Inner<A> {
    async fn plan_record(&self) -> Result<Option<Record>, Error> {
        let stored = rows(
            &self.conn,
            "SELECT descriptor,domain,readers,position,cursor FROM secure_projection_plan WHERE id=1",
        )
        .await?;
        let Some(row) = stored.first() else {
            return Ok(None);
        };
        let [
            SqlValue::Blob(descriptor),
            SqlValue::Blob(domain),
            SqlValue::Blob(readers),
            SqlValue::Integer(position),
            SqlValue::Blob(cursor),
        ] = row.as_slice()
        else {
            return Err(Error::Corrupt("projection plan record".into()));
        };
        Ok(Some(Record {
            schema: LogicalSchema::parse(descriptor)
                .map_err(|_| Error::Corrupt("projection plan descriptor".into()))?,
            domain: decode(domain)?,
            readers: decode(readers)?,
            position: *position,
            cursor: cursor.clone(),
        }))
    }
    /// The installed typed descriptor, domain and readers.
    pub(super) async fn installed_typed(
        &self,
    ) -> Result<Option<(LogicalSchema, Scope, BTreeMap<String, Commitment>)>, Error> {
        let stored = rows(
            &self.conn,
            "SELECT descriptor,domain,readers FROM secure_typed WHERE id=1",
        )
        .await?;
        let Some(row) = stored.first() else {
            return Ok(None);
        };
        let [
            SqlValue::Blob(descriptor),
            SqlValue::Blob(domain),
            SqlValue::Blob(readers),
        ] = row.as_slice()
        else {
            return Err(Error::Corrupt("typed table record".into()));
        };
        Ok(Some((
            LogicalSchema::parse(descriptor)
                .map_err(|_| Error::Corrupt("typed table descriptor".into()))?,
            decode(domain)?,
            decode(readers)?,
        )))
    }
    /// Checks that `schema` may replace the installed typed tables and
    /// records the plan, inside the caller's transaction. The identical plan
    /// is kept with its progress; another one pending is refused.
    pub(super) async fn record_plan(
        &self,
        schema: &LogicalSchema,
        domain: Scope,
        readers: &BTreeMap<String, Commitment>,
    ) -> Result<(), Error> {
        let incompatible = |reason: String| Error::Incompatible(reason);
        let descriptor = schema
            .canonical()
            .map_err(|error| incompatible(error.to_string()))?;
        if let Some(pending) = self.plan_record().await? {
            if pending.schema == *schema && pending.domain == domain && pending.readers == *readers
            {
                return Ok(());
            }
            return Err(incompatible(
                "another projection plan is pending; reopen to finish it first".into(),
            ));
        }
        let Some((installed, installed_domain, installed_readers)) = self.installed_typed().await?
        else {
            return Err(incompatible("no typed tables are installed".into()));
        };
        if installed_domain != domain
            || installed_readers
                .iter()
                .any(|(name, reader)| readers.get(name) != Some(reader))
        {
            return Err(incompatible(
                "a projection plan keeps the domain and reader scopes".into(),
            ));
        }
        match rocky_core::classify(&installed, schema)
            .map_err(|refusal| incompatible(refusal.to_string()))?
        {
            SchemaChange::Unchanged => return Ok(()),
            SchemaChange::Local | SchemaChange::Additive { .. } | SchemaChange::Breaking { .. } => {
            }
        }
        // Local tables hold application data no stored row restores, and
        // confirmed rows change only through commands: both stay in place.
        for table in installed.tables.iter().filter(|table| {
            matches!(
                table.class,
                TableClass::LocalOnly | TableClass::CommandControlled
            )
        }) {
            if schema.table(&table.sql_name).is_none_or(|new| {
                new.class != table.class
                    || new.create_sql != table.create_sql
                    || new.logical != table.logical
            }) {
                return Err(incompatible(format!(
                    "{} table {} must stay as it is; only its indexes may change",
                    if table.class == TableClass::LocalOnly {
                        "local"
                    } else {
                        "command-controlled"
                    },
                    table.sql_name
                )));
            }
        }
        if !constrained(&installed, schema, domain, readers)? {
            Typed::shadow(schema.clone(), domain, readers)?;
        }
        self.conn
            .execute(
                "INSERT INTO secure_projection_plan VALUES(1,?,?,?,-1,X'')",
                bindings(vec![
                    SqlValue::Blob(descriptor),
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Blob(encode(readers)?),
                ]),
            )
            .await?;
        Ok(())
    }
    /// The pending plan's progress.
    pub(super) async fn plan_status(&self) -> Result<Option<ProjectionPlan>, Error> {
        let Some(record) = self.plan_record().await? else {
            return Ok(None);
        };
        let tables = record.schema.mergeable().count();
        Ok(Some(ProjectionPlan {
            schema: record
                .schema
                .fingerprint()
                .map_err(|error| Error::Corrupt(error.to_string()))?,
            filled: usize::try_from(record.position.max(0))
                .unwrap_or_default()
                .min(tables),
            tables,
        }))
    }
    /// A bulk change of a scope's stored rows: its shadow fills again.
    pub(super) async fn restart_plan(&self, scope: ResourceScope) -> Result<(), Error> {
        let Some(shadow) = &self.shadow else {
            return Ok(());
        };
        let Some(index) = shadow.tables.iter().position(|table| table.scope == scope) else {
            return Ok(());
        };
        let table = &shadow.tables[index];
        self.conn
            .execute(
                format!(
                    "DELETE FROM {}",
                    quoted(&physical(&table.table.sql_name, true))
                ),
                (),
            )
            .await?;
        self.conn
            .execute(
                "UPDATE secure_projection_plan SET position=?,cursor=X'' WHERE position>?",
                [index as i64, index as i64],
            )
            .await?;
        Ok(())
    }
    /// Runs one step of the pending plan in its own transaction. Returns
    /// whether a plan remains.
    pub(super) async fn plan_step(&mut self) -> Result<bool, Error> {
        let Some(record) = self.plan_record().await? else {
            return Ok(false);
        };
        let installed = self
            .installed_typed()
            .await?
            .ok_or_else(|| Error::Corrupt("a projection plan without typed tables".into()))?
            .0;
        if constrained(&installed, &record.schema, record.domain, &record.readers)? {
            self.switch_constrained(&record, &installed).await?;
            return Ok(false);
        }
        let shadow = Arc::new(
            self.authorized_projection(Typed::shadow(
                record.schema.clone(),
                record.domain,
                &record.readers,
            )?)
            .await?,
        );
        let tables = shadow.tables.len() as i64;
        let switched = transaction!(self, |inner| {
            match record.position {
                -1 => {
                    inner.plan_fault(PlanStage::Create)?;
                    let names = Names::new(Some(SHADOW), &statements(&record.schema));
                    for table in &shadow.tables {
                        let name = &table.table.sql_name;
                        inner
                            .conn
                            .execute(
                                format!("DROP TABLE IF EXISTS {}", quoted(&physical(name, true))),
                                (),
                            )
                            .await?;
                        let logical = record
                            .schema
                            .table(name)
                            .ok_or_else(|| Error::Corrupt(format!("{name} has no statement")))?;
                        inner
                            .conn
                            .execute(names.sql(&logical.create_sql), ())
                            .await?;
                    }
                    inner
                        .conn
                        .execute("UPDATE secure_projection_plan SET position=0", ())
                        .await?;
                    Ok(false)
                }
                position if position < tables => {
                    inner.plan_fault(PlanStage::Fill)?;
                    let table = &shadow.tables[position as usize];
                    let (copied, last) = inner.fill(&shadow, table, &record.cursor).await?;
                    let (position, cursor) = if copied < PLAN_ROWS {
                        (position + 1, Vec::new())
                    } else {
                        (position, last)
                    };
                    inner
                        .conn
                        .execute(
                            "UPDATE secure_projection_plan SET position=?,cursor=?",
                            bindings(vec![SqlValue::Integer(position), SqlValue::Blob(cursor)]),
                        )
                        .await?;
                    Ok(false)
                }
                _ => {
                    inner.plan_fault(PlanStage::Switch)?;
                    inner.switch(&record, &shadow).await?;
                    Ok(true)
                }
            }
        })?;
        if switched {
            self.shadow = None;
            self.typed = Some(Arc::new(
                self.authorized_projection(Typed::new(
                    record.schema,
                    record.domain,
                    &record.readers,
                )?)
                .await?,
            ));
            self.start_events();
            // Sessions and cursors planned against the old tables end.
            self.bump_authority();
            self.publish_revision().await?;
            return Ok(false);
        }
        if self.shadow.is_none() {
            self.shadow = Some(shadow);
        }
        Ok(true)
    }
    /// Replaces tables whose visible rows depend on unique indexes or foreign
    /// keys: every candidate decides them, so instead of filling shadows the
    /// tables are recreated and resolved from the stored rows in one
    /// transaction, as at installation.
    async fn switch_constrained(
        &mut self,
        record: &Record,
        installed: &LogicalSchema,
    ) -> Result<(), Error> {
        self.plan_fault(PlanStage::Switch)?;
        let projected = Arc::new(
            self.authorized_projection(Typed::new(
                record.schema.clone(),
                record.domain,
                &record.readers,
            )?)
            .await?,
        );
        // Resolution reads the installed projection.
        let previous = self.typed.replace(projected.clone());
        let switched = transaction!(self, |inner| {
            for table in installed.mergeable() {
                inner.touch(&table.sql_name);
                for name in [
                    table.sql_name.clone(),
                    format!("secure_candidates_{}", table.sql_name),
                ] {
                    inner
                        .conn
                        .execute(format!("DROP TABLE IF EXISTS {}", quoted(&name)), ())
                        .await?;
                }
            }
            inner.conn.execute("DELETE FROM secure_support", ()).await?;
            inner.record_projection_access(&projected).await?;
            for table in record.schema.tables.iter().filter(|table| {
                table.class == TableClass::LocalOnly || projected.by_name(&table.sql_name).is_some()
            }) {
                inner.touch(&table.sql_name);
                if table.class == TableClass::Mergeable {
                    inner.conn.execute(table.create_sql.clone(), ()).await?;
                    for index in &table.indexes {
                        inner.conn.execute(index.create_sql.clone(), ()).await?;
                    }
                } else {
                    inner.replace_indexes(installed, table).await?;
                }
            }
            for sql in projected
                .constraints
                .iter()
                .flat_map(super::constraints::Constraints::statements)
            {
                inner.conn.execute(sql.clone(), ()).await?;
            }
            for table in &projected.tables {
                if !table.confirmed()
                    && projected
                        .constraints
                        .as_ref()
                        .and_then(|constraints| constraints.node(table.scope))
                        .is_none()
                {
                    typed::reproject(&inner.conn, &projected, table).await?;
                }
            }
            Box::pin(inner.rebuild_constraints(None, None)).await?;
            typed::verify_constraint_indexes(&inner.conn, &projected).await?;
            inner.finish_switch(record).await
        });
        if let Err(error) = switched {
            self.typed = previous;
            return Err(error);
        }
        self.start_events();
        self.bump_authority();
        self.publish_revision().await?;
        Ok(())
    }
    /// Runs the pending plan to its switch.
    pub(super) async fn finish_plan(&mut self) -> Result<(), Error> {
        while self.plan_step().await? {}
        Ok(())
    }
    /// Copies one batch of a scope's live rows after `cursor` into its
    /// shadow table. Rows that change meanwhile project there themselves.
    async fn fill(
        &self,
        shadow: &Typed,
        table: &typed::Projected,
        cursor: &[u8],
    ) -> Result<(usize, Vec<u8>), Error> {
        let chunk = collect_rows(
            self.conn
                .query(
                    "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                    bindings(vec![
                        SqlValue::Blob(encode(&table.scope)?),
                        SqlValue::Blob(cursor.to_vec()),
                        SqlValue::Integer(PLAN_ROWS as i64),
                    ]),
                )
                .await?,
        )
        .await?;
        let mut last = cursor.to_vec();
        for record in &chunk {
            let [SqlValue::Blob(row), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("stored row".into()));
            };
            last = row.clone();
            let record: Materialized = decode(bytes)?;
            if record.deleted
                || (record.identity.is_some()
                    && self
                        .winner(
                            table.scope,
                            record
                                .key
                                .ok_or_else(|| Error::Corrupt("missing incarnation key".into()))?,
                        )
                        .await?
                        .is_none_or(|winner| winner.identity != record.identity))
            {
                continue;
            }
            let values = shadow
                .rows
                .values(&table.table, &record.fields)
                .map_err(Error::Corrupt)?;
            self.conn
                .execute(table.upsert.clone(), bindings(values))
                .await?;
        }
        Ok((chunk.len(), last))
    }
    /// Verifies every shadow table against the stored rows, then replaces
    /// the installed tables with them, inside the caller's transaction.
    async fn switch(&self, record: &Record, shadow: &Typed) -> Result<(), Error> {
        for table in &shadow.tables {
            self.verify_shadow(shadow, table).await?;
        }
        let installed = self
            .installed_typed()
            .await?
            .ok_or_else(|| Error::Corrupt("a projection plan without typed tables".into()))?
            .0;
        for table in installed.mergeable() {
            self.touch(&table.sql_name);
            self.conn
                .execute(
                    format!("DROP TABLE IF EXISTS {}", quoted(&table.sql_name)),
                    (),
                )
                .await?;
        }
        let projected = self
            .authorized_projection(Typed::new(
                record.schema.clone(),
                record.domain,
                &record.readers,
            )?)
            .await?;
        self.record_projection_access(&projected).await?;
        for table in record.schema.tables.iter().filter(|table| {
            table.class == TableClass::LocalOnly || projected.by_name(&table.sql_name).is_some()
        }) {
            self.touch(&table.sql_name);
            match table.class {
                TableClass::Mergeable => {
                    self.conn
                        .execute(
                            format!(
                                "ALTER TABLE {} RENAME TO {}",
                                quoted(&physical(&table.sql_name, true)),
                                quoted(&table.sql_name)
                            ),
                            (),
                        )
                        .await?;
                    for index in &table.indexes {
                        self.conn.execute(index.create_sql.clone(), ()).await?;
                    }
                }
                _ => self.replace_indexes(&installed, table).await?,
            }
        }
        self.finish_switch(record).await
    }
    /// A local or command-controlled table keeps its rows; only its indexes
    /// follow the new schema.
    async fn replace_indexes(
        &self,
        installed: &LogicalSchema,
        table: &rocky_core::LogicalTable,
    ) -> Result<(), Error> {
        let before = installed.table(&table.sql_name);
        if before.is_none() {
            self.conn.execute(table.create_sql.clone(), ()).await?;
        }
        let had = |name: &str, sql: &str| {
            before.is_some_and(|before| {
                before
                    .indexes
                    .iter()
                    .any(|index| index.name == name && index.create_sql == sql)
            })
        };
        for index in before.iter().flat_map(|before| &before.indexes) {
            if !table
                .indexes
                .iter()
                .any(|new| new.name == index.name && new.create_sql == index.create_sql)
            {
                self.conn
                    .execute(format!("DROP INDEX {}", quoted(&index.name)), ())
                    .await?;
            }
        }
        for index in &table.indexes {
            if !had(&index.name, &index.create_sql) {
                self.conn.execute(index.create_sql.clone(), ()).await?;
            }
        }
        Ok(())
    }
    /// Records the new tables as installed and ends the plan.
    async fn finish_switch(&self, record: &Record) -> Result<(), Error> {
        let descriptor = record
            .schema
            .canonical()
            .map_err(|error| Error::Incompatible(error.to_string()))?;
        self.conn
            .execute(
                "UPDATE secure_typed SET descriptor=?,domain=?,readers=? WHERE id=1",
                bindings(vec![
                    SqlValue::Blob(descriptor),
                    SqlValue::Blob(encode(&record.domain)?),
                    SqlValue::Blob(encode(&record.readers)?),
                ]),
            )
            .await?;
        self.conn
            .execute("DELETE FROM secure_projection_plan", ())
            .await?;
        self.conn
            .execute(
                "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                (),
            )
            .await?;
        self.cause(crate::observe::Cause::Rebuild);
        Ok(())
    }
    /// The shadow table holds exactly the live stored rows of its scope.
    async fn verify_shadow(&self, shadow: &Typed, table: &typed::Projected) -> Result<(), Error> {
        let differs = || {
            Error::Corrupt(format!(
                "the shadow of {} differs from its stored rows",
                table.table.sql_name
            ))
        };
        let (mut live, mut after) = (0i64, Vec::new());
        loop {
            let chunk = collect_rows(
                self.conn
                    .query(
                        "SELECT row,fields FROM secure_rows WHERE scope=? AND row>? ORDER BY row LIMIT ?",
                        bindings(vec![
                            SqlValue::Blob(encode(&table.scope)?),
                            SqlValue::Blob(after.clone()),
                            SqlValue::Integer(PLAN_ROWS as i64),
                        ]),
                    )
                    .await?,
            )
            .await?;
            for record in &chunk {
                let [SqlValue::Blob(row), SqlValue::Blob(bytes)] = record.as_slice() else {
                    return Err(Error::Corrupt("stored row".into()));
                };
                after = row.clone();
                let record: Materialized = decode(bytes)?;
                if record.deleted
                    || (record.identity.is_some()
                        && self
                            .winner(
                                table.scope,
                                record.key.ok_or_else(|| {
                                    Error::Corrupt("missing incarnation key".into())
                                })?,
                            )
                            .await?
                            .is_none_or(|winner| winner.identity != record.identity))
                {
                    continue;
                }
                live += 1;
                let values = shadow
                    .rows
                    .values(&table.table, &record.fields)
                    .map_err(Error::Corrupt)?;
                let key: Vec<SqlValue> = table
                    .table
                    .key
                    .iter()
                    .filter_map(|index| values.get(*index).cloned())
                    .collect();
                let stored =
                    collect_rows(self.conn.query(table.select.clone(), bindings(key)).await?)
                        .await?;
                if stored.first() != Some(&values) {
                    return Err(differs());
                }
            }
            if chunk.len() < PLAN_ROWS {
                break;
            }
        }
        let counted = rows(
            &self.conn,
            &format!(
                "SELECT count(*) FROM {}",
                quoted(&physical(&table.table.sql_name, true))
            ),
        )
        .await?;
        if counted != vec![vec![SqlValue::Integer(live)]] {
            return Err(differs());
        }
        Ok(())
    }
    /// A test's interruption of a plan step.
    #[cfg_attr(not(test), allow(clippy::unused_self, unused_variables))]
    fn plan_fault(&mut self, stage: PlanStage) -> Result<(), Error> {
        #[cfg(test)]
        if let Some((at, remaining)) = self.plan_fault
            && at == stage
        {
            if remaining == 0 {
                self.plan_fault = None;
                return Err(Error::InjectedFailure);
            }
            self.plan_fault = Some((at, remaining - 1));
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The pending projection plan, if a typed schema change is under way.
    pub async fn projection_plan(&self) -> Result<Option<ProjectionPlan>, Error> {
        self.inner.lock().await.plan_status().await
    }
    /// Runs a pending plan step by step; other work proceeds between steps.
    pub(super) async fn run_plan(&self) -> Result<(), Error> {
        loop {
            let mut inner = self.inner.lock().await;
            if !inner.plan_step().await? {
                return Ok(());
            }
        }
    }
    #[cfg(test)]
    pub(crate) async fn inject_plan_fault(&self, stage: PlanStage, after: usize) {
        self.inner.lock().await.plan_fault = Some((stage, after));
    }
}

/// Whether either schema's tables carry unique indexes or foreign keys.
fn constrained(
    installed: &LogicalSchema,
    schema: &LogicalSchema,
    domain: Scope,
    readers: &BTreeMap<String, Commitment>,
) -> Result<bool, Error> {
    Ok(installed
        .tables
        .iter()
        .any(|table| installed.constrained(table))
        || Typed::new(schema.clone(), domain, readers)?
            .constraints
            .is_some())
}
