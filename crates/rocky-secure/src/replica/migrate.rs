use super::replay::{BrokenReference, Rebuilding, column_carried, table_carried};
use super::typed::{Typed, quoted};
use super::*;
use rocky_core::{LogicalSchema, SchemaChange, Scope, TableClass};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_projection_plan(id INTEGER PRIMARY KEY CHECK(id=1),descriptor BLOB NOT NULL,domain BLOB NOT NULL,readers BLOB NOT NULL)",
];

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProjectionPlan {
    pub schema: Commitment,
    pub tables: usize,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum PlanCause {
    Install,
    Switch,
    Withdrawal,
}

struct Record {
    schema: LogicalSchema,
    domain: Scope,
    readers: BTreeMap<String, Commitment>,
}

impl<A: RowApplication> Inner<A> {
    async fn plan_record(&self) -> Result<Option<Record>, Error> {
        let stored = rows(
            &self.conn,
            "SELECT descriptor,domain,readers FROM secure_projection_plan WHERE id=1",
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
            return Err(Error::Corrupt("projection plan record".into()));
        };
        Ok(Some(Record {
            schema: stored_schema(descriptor, "projection plan descriptor")?,
            domain: decode(domain)?,
            readers: decode(readers)?,
        }))
    }
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
            stored_schema(descriptor, "typed table descriptor")?,
            decode(domain)?,
            decode(readers)?,
        )))
    }
    pub(super) async fn record_plan(
        &self,
        schema: &LogicalSchema,
        domain: Scope,
        readers: &BTreeMap<String, Commitment>,
        cause: PlanCause,
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
        let kept_table_changes_reader = installed_readers.iter().any(|(name, reader)| {
            readers.get(name).is_some_and(|new| new != reader)
                || (readers.get(name).is_none() && schema.table(name).is_some())
        });
        if installed_domain != domain || kept_table_changes_reader {
            return Err(incompatible(
                "a projection plan keeps the domain and reader scopes".into(),
            ));
        }
        let switch_path = self.switch_path_fields().await?;
        let switch_path = switch_path.as_ref();
        let carried = |table: &rocky_core::LogicalTable,
                       column: Option<&rocky_core::LogicalColumn>| {
            match column {
                None => table_carried(switch_path, &installed, table),
                Some(column) => {
                    let rebuilt = schema
                        .table(&table.sql_name)
                        .and_then(|rebuilt| rebuilt.column(&column.sql_name))
                        .unwrap_or(column);
                    column_carried(switch_path, &installed, table, column, rebuilt)
                }
            }
        };
        let change = match cause {
            PlanCause::Withdrawal => rocky_core::classify_withdrawal(&installed, schema, carried),
            PlanCause::Install | PlanCause::Switch => {
                rocky_core::classify_carrying(&installed, schema, carried)
            }
        };
        match change.map_err(|refusal| incompatible(refusal.to_string()))? {
            SchemaChange::Unchanged if cause == PlanCause::Install => return Ok(()),
            SchemaChange::Unchanged | SchemaChange::Local | SchemaChange::Replicated => {}
        }
        for table in installed
            .tables
            .iter()
            .filter(|table| table.class == TableClass::LocalOnly)
        {
            if schema
                .table(&table.sql_name)
                .is_none_or(|new| new.class != table.class || new.create_sql != table.create_sql)
            {
                return Err(incompatible(format!(
                    "local table {} must stay as it is; only its indexes may change",
                    table.sql_name
                )));
            }
        }
        self.conn
            .execute(
                "INSERT INTO secure_projection_plan VALUES(1,?,?,?)",
                bindings(vec![
                    SqlValue::Blob(descriptor),
                    SqlValue::Blob(encode(&domain)?),
                    SqlValue::Blob(encode(readers)?),
                ]),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn projection_grew(&self, typed: &Typed) -> Result<bool, Error> {
        for table in &typed.tables {
            let mut found = self
                .conn
                .query(
                    "SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1 COLLATE NOCASE",
                    [table.table.sql_name.clone()],
                )
                .await?;
            if found.next().await?.is_none() {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) async fn record_growth(&self) -> Result<(), Error> {
        if self.plan_record().await?.is_some() {
            return Ok(());
        }
        self.conn
            .execute(
                "INSERT INTO secure_projection_plan SELECT 1,descriptor,domain,readers FROM secure_typed WHERE id=1",
                (),
            )
            .await?;
        Ok(())
    }
    pub(super) async fn discard_plan(&mut self) -> Result<(), Error> {
        self.conn
            .execute("DELETE FROM secure_projection_plan", ())
            .await?;
        Ok(())
    }
    pub(super) async fn plan_status(&self) -> Result<Option<ProjectionPlan>, Error> {
        let Some(record) = self.plan_record().await? else {
            return Ok(None);
        };
        Ok(Some(ProjectionPlan {
            schema: record
                .schema
                .fingerprint()
                .map_err(|error| Error::Corrupt(error.to_string()))?,
            tables: record.schema.typed().count(),
        }))
    }
    async fn switch_path_fields(&self) -> Result<Option<super::schemas::PathFields>, Error> {
        match self.schema_switch_intent().await? {
            Some((scope, ..)) => self.staged_row_fields(scope).await,
            None => Ok(None),
        }
    }
    pub(super) async fn finish_plan(&mut self) -> Result<Result<(), BrokenReference>, Error> {
        let Some(record) = self.plan_record().await? else {
            return Ok(Ok(()));
        };
        let intent = self.schema_switch_intent().await?;
        if let Some((scope, _, _, target)) = intent
            && self
                .graph_for(scope)?
                .schema_any(target)
                .map_err(credential)?
                != self.manifest
        {
            return Err(denied(
                "a staged schema switch resumes only with its target schema",
            ));
        }
        let fields_on_switch_path = self.switch_path_fields().await?;
        let installed = self
            .installed_typed()
            .await?
            .ok_or_else(|| Error::Corrupt("a projection plan without typed tables".into()))?
            .0;
        #[cfg(test)]
        if std::mem::take(&mut self.plan_fault) {
            return Err(Error::InjectedFailure);
        }
        let projected = Arc::new(
            self.authorized_projection(Typed::new(
                record.schema.clone(),
                record.domain,
                &record.readers,
            )?)
            .await?,
        );
        let previous = self.workspace.typed.replace(projected.clone());
        let mut broken = None;
        let switched = transaction!(self, |inner| {
            let switching = match &previous {
                Some(old) => inner.begin_switch_history(old, &projected).await?,
                None => None,
            };
            let removed = removed_replayed(&installed, &record.schema)
                .next()
                .is_some();
            for table in removed_replayed(&installed, &record.schema) {
                inner.touch(&table.sql_name);
                inner
                    .conn
                    .execute(
                        format!("DROP TABLE IF EXISTS {}", quoted(&table.sql_name)),
                        (),
                    )
                    .await?;
            }
            inner.record_projection_access(&projected).await?;
            let mut replayed = Vec::new();
            for table in record.schema.tables.iter().filter(|table| {
                table.class == TableClass::LocalOnly || projected.by_name(&table.sql_name).is_some()
            }) {
                inner.touch(&table.sql_name);
                match table.class {
                    TableClass::Replayed => replayed.push(table),
                    TableClass::LocalOnly => inner.replace_indexes(&installed, table).await?,
                }
            }
            if let Err(refused) = inner
                .rebuild_replayed(
                    &installed,
                    &record.schema,
                    &replayed,
                    fields_on_switch_path.as_ref(),
                )
                .await?
            {
                broken = Some(refused.clone());
                return Err(refused.refuses(Rebuilding::SchemaProjection));
            }
            inner.install_delete_triggers(&projected).await?;
            inner
                .reshape_undo(&installed, &record.schema, fields_on_switch_path.as_ref())
                .await?;
            if removed {
                inner.redo_removed_writes(now()?).await?;
            }
            inner.install_history().await?;
            inner.end_switch_history(&projected, switching).await?;
            #[cfg(test)]
            if std::mem::take(&mut inner.switch_fault) {
                return Err(Error::Incompatible(
                    "an injected incompatible switch".into(),
                ));
            }
            inner.finish_switch(&record).await
        });
        if let Err(error) = switched {
            self.abandon_switch_history();
            self.workspace.typed = previous;
            return match broken {
                Some(refused) => Ok(Err(refused)),
                None => Err(error),
            };
        }
        self.start_events();
        self.bump_authority();
        self.publish_revision().await?;
        Ok(Ok(()))
    }
    pub(super) async fn finish_or_discard_plan(
        &mut self,
    ) -> Result<Result<(), BrokenReference>, Error> {
        let finished = self.finish_plan().await?;
        if finished.is_err() {
            transaction!(self, |inner| inner.discard_plan().await)?;
        }
        Ok(finished)
    }
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
    async fn finish_switch(&mut self, record: &Record) -> Result<(), Error> {
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
        self.finish_schema_switch().await?;
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn projection_plan(&self) -> Result<Option<ProjectionPlan>, Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .plan_status()
            .await
    }
    pub(crate) async fn run_plan(&self) -> Result<(), Error> {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .finish_or_discard_plan()
            .await?
            .map_err(|broken| broken.refuses(Rebuilding::SchemaProjection))
    }
    #[cfg(test)]
    pub(crate) async fn fail_next_plan_switch(&self) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .plan_fault = true;
    }
    #[cfg(test)]
    pub(crate) async fn refuse_next_switch_as_incompatible(&self) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .switch_fault = true;
    }
}

fn removed_replayed<'a>(
    installed: &'a LogicalSchema,
    schema: &'a LogicalSchema,
) -> impl Iterator<Item = &'a rocky_core::LogicalTable> {
    installed.tables.iter().filter(|table| {
        table.class == TableClass::Replayed && schema.table(&table.sql_name).is_none()
    })
}

pub(super) fn stored_schema(descriptor: &[u8], record: &str) -> Result<LogicalSchema, Error> {
    LogicalSchema::parse(descriptor).map_err(|refusal| {
        let well_formed = serde_json::from_slice::<LogicalSchema>(descriptor)
            .ok()
            .and_then(|schema| serde_json::to_vec(&schema).ok())
            .is_some_and(|reencoded| reencoded == descriptor);
        if well_formed {
            Error::Incompatible(format!(
                "the stored {record} no longer validates: {refusal}"
            ))
        } else {
            Error::Corrupt(record.into())
        }
    })
}
