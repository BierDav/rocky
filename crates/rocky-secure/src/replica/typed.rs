use super::*;
use crate::{capture::Plan, scoped_connection::Names};
use rocky_core::{LogicalSchema, Scope, TableClass, TypedRows, TypedTable};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_projection_access(id INTEGER PRIMARY KEY CHECK(id=1),tables BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_typed(id INTEGER PRIMARY KEY CHECK(id=1),descriptor BLOB NOT NULL,domain BLOB NOT NULL,readers BLOB NOT NULL)",
];

#[derive(Clone)]
pub(super) struct Projected {
    pub(super) table: TypedTable,
    pub(super) scope: ResourceScope,
}
pub(super) struct Typed {
    pub(super) rows: TypedRows,
    pub(super) domain: Scope,
    pub(super) tables: Vec<Projected>,
    pub(super) declarations: Vec<Projected>,
    pub(super) names: Names,
    pub(super) whole_tables: rocky_credentials::WholeTables,
}
pub(super) fn quoted(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}
impl Typed {
    pub(super) fn new(
        schema: LogicalSchema,
        domain: Scope,
        readers: &BTreeMap<String, Commitment>,
    ) -> Result<Self, Error> {
        let refused =
            |reason: String| Error::Application(Box::new(rocky_core::SchemaError::Invalid(reason)));
        let rows = TypedRows::new(schema.clone(), vec![domain.domain])
            .map_err(|error| Error::Application(Box::new(error)))?;
        let mut tables = Vec::new();
        let typed: Vec<_> = rows.tables().collect();
        for table in typed.iter() {
            if schema.table(&table.sql_name).is_none() {
                return Err(refused(format!("{} is not in the schema", table.sql_name)));
            }
            let encryption = readers
                .get(&table.sql_name)
                .ok_or_else(|| refused(format!("{} has no reader scope", table.sql_name)))?;
            tables.push(Projected {
                scope: ResourceScope {
                    domain,
                    encryption: *encryption,
                    table: table.id,
                },
                table: (*table).clone(),
            });
        }
        if readers.len() != typed.len() {
            return Err(refused("a reader scope names no typed table".into()));
        }
        let whole_tables = rocky_credentials::WholeTables::new(
            &rows.manifest(),
            &tables.iter().map(|table| table.scope).collect::<Vec<_>>(),
        )
        .map_err(credential)?;
        let statements = statements(&schema);
        Ok(Self {
            whole_tables,
            names: Names::new(None, &statements),
            rows,
            domain,
            declarations: tables.clone(),
            tables,
        })
    }
    pub(super) fn restrict(mut self, allowed: &BTreeSet<String>) -> Self {
        self.tables
            .retain(|table| allowed.contains(&table.table.sql_name));
        self
    }
    pub(super) fn statements(&self) -> Vec<String> {
        let schema = self.rows.schema();
        let mut statements: Vec<_> = schema
            .tables
            .iter()
            .filter(|table| self.installs(&table.sql_name))
            .flat_map(|table| {
                std::iter::once(table.create_sql.clone())
                    .chain(table.indexes.iter().map(|index| index.create_sql.clone()))
            })
            .collect();
        statements.extend(schema.delete_triggers(|name| self.installs(name)));
        statements
    }
    pub(super) fn installs(&self, name: &str) -> bool {
        self.rows.schema().table(name).is_some_and(|table| {
            table.class == TableClass::LocalOnly || self.by_name(&table.sql_name).is_some()
        })
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
        partitions: &BTreeMap<ResourceScope, rocky_core::PartitionTuple>,
    ) -> Result<ResourceScope, Error> {
        self.registered_row_scope(table, values, partitions)?
            .ok_or_else(|| denied(super::partition::UNREGISTERED_PARTITION))
    }
    pub(super) fn registered_row_scope(
        &self,
        table: &Projected,
        values: &[SqlValue],
        partitions: &BTreeMap<ResourceScope, rocky_core::PartitionTuple>,
    ) -> Result<Option<ResourceScope>, Error> {
        match self
            .rows
            .partition(&table.table, values)
            .map_err(|error| denied(&error))?
        {
            Some(tuple) => Ok(super::partition::registered_scope(
                partitions,
                self.domain,
                table.table.id,
                &tuple,
            )),
            None => Ok(Some(table.scope)),
        }
    }
    pub(super) fn by_scope(&self, scope: ResourceScope) -> Option<&Projected> {
        self.covering(&self.tables, scope)
    }
    pub(super) fn by_name(&self, sql_name: &str) -> Option<&Projected> {
        self.tables
            .iter()
            .find(|table| table.table.sql_name.eq_ignore_ascii_case(sql_name))
    }
    pub(super) fn declared_by_scope(&self, scope: ResourceScope) -> Option<&Projected> {
        self.covering(&self.declarations, scope)
    }
    fn covering<'a>(&self, tables: &'a [Projected], scope: ResourceScope) -> Option<&'a Projected> {
        tables.iter().find(|table| {
            table.scope == scope
                || (table.scope.domain == scope.domain
                    && table.scope.table == scope.table
                    && self.partitioned(table))
        })
    }
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
        let tables: Vec<&str> = schema
            .tables
            .iter()
            .filter(|table| named(table))
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
    pub async fn install_readable_tables(
        &self,
        schema: LogicalSchema,
        domain: Scope,
        readers: BTreeMap<String, Commitment>,
    ) -> Result<(), Error> {
        {
            let mut inner = self.inner.lock(crate::replica::Class::Background).await;
            inner.validate_typed(&schema, domain)?;
            let typed = Typed::new(schema.clone(), domain, &readers)?;
            let allowed = inner.readable_names(&typed)?;
            inner.forget_unreadable(Some(&allowed)).await?;
            transaction!(inner, |inner| {
                let previous = inner.projection_access().await?;
                if previous.is_none() && inner.installed_typed().await?.is_some() {
                    return Err(Error::Incompatible("a complete projection cannot become a readable projection; reinstall the database".into()));
                }
                inner.conn.execute("INSERT INTO secure_projection_access VALUES(1,?) ON CONFLICT(id) DO UPDATE SET tables=excluded.tables", [encode(&allowed)?]).await?;
                Ok(())
            })?;
        }
        self.install_tables(schema, domain, readers).await
    }
    pub async fn install_tables(
        &self,
        schema: LogicalSchema,
        domain: Scope,
        readers: BTreeMap<String, Commitment>,
    ) -> Result<(), Error> {
        schema
            .require_row_identity()
            .map_err(|error| Error::Application(Box::new(error)))?;
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.validate_typed(&schema, domain)?;
        let typed = inner
            .authorized_projection(Typed::new(schema.clone(), domain, &readers)?)
            .await?;
        let record = [
            schema
                .canonical()
                .map_err(|error| Error::Application(Box::new(error)))?,
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
                Some(row) if *row == expected => {
                    if inner.projection_grew(&typed).await? {
                        inner.record_growth().await?;
                        return Ok(true);
                    }
                    return Ok(false);
                }
                Some(_) => {
                    inner
                        .record_plan(
                            &schema,
                            domain,
                            &readers,
                            super::migrate::PlanCause::Install,
                        )
                        .await?;
                    return Ok(true);
                }
                None => {}
            }
            for sql in typed.statements() {
                inner.conn.execute(sql, ()).await?;
            }
            inner
                .conn
                .execute(
                    "INSERT INTO secure_typed VALUES(1,?,?,?)",
                    bindings(expected),
                )
                .await?;
            Ok(false)
        })?;
        if planned {
            drop(inner);
            return self.run_plan().await;
        }
        inner.workspace.typed = Some(Arc::new(typed));
        inner.start_events();
        Ok(())
    }
}

impl<A: RowApplication> Inner<A> {
    fn validate_typed(&self, schema: &LogicalSchema, domain: Scope) -> Result<(), Error> {
        schema
            .require_row_identity()
            .map_err(|error| Error::Application(Box::new(error)))?;
        let domains: Vec<_> = self
            .definition
            .domains
            .iter()
            .map(|rule| rule.domain)
            .collect();
        if domain.app != self.definition.app || !domains.contains(&domain.domain) {
            return Err(denied("typed tables need a domain of the signed manifest"));
        }
        let expected = schema.manifest_at(&domains, self.definition.schema, self.definition.engine);
        if expected.hash() != self.manifest {
            return Err(Error::Application(Box::new(
                rocky_core::SchemaError::Invalid(
                    "the schema's definition differs from the signed manifest".into(),
                ),
            )));
        }
        if schema
            .tables
            .iter()
            .any(|table| table.sql_name.to_ascii_lowercase().starts_with("secure_"))
        {
            return Err(Error::Application(Box::new(
                rocky_core::SchemaError::Invalid("secure_ names are reserved".into()),
            )));
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
            .verified(
                self.config.publisher,
                &self.config.revocations,
                now()?,
                &typed.whole_tables,
            )
            .map_err(credential)?
            .selectors(
                self.config.node.public_key(),
                &self.config.revocations,
                now()?,
            )
            .map_err(credential)?;
        let allowed: BTreeSet<String> = typed
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
        let schema = typed.rows.schema();
        for table in &typed.tables {
            if !allowed.contains(&table.table.sql_name) {
                continue;
            }
            let Some(logical) = schema.table(&table.table.sql_name) else {
                continue;
            };
            for key in &logical.foreign_keys {
                if let Some(parent) = schema.table(&key.table)
                    && parent.class == TableClass::Replayed
                    && !allowed.contains(&parent.sql_name)
                {
                    return Err(denied(&format!(
                        "readable table {} references {} which is not granted for read",
                        logical.sql_name, parent.sql_name
                    )));
                }
            }
        }
        Ok(allowed)
    }
    pub(super) async fn authorized_projection(&self, typed: Typed) -> Result<Typed, Error> {
        match self.projection_access().await? {
            Some(_) => {
                let allowed = self.readable_names(&typed)?;
                Ok(typed.restrict(&allowed))
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
        let schema = super::migrate::stored_schema(descriptor, "typed table descriptor")?;
        let mut typed = self
            .authorized_projection(Typed::new(schema, decode(domain)?, &decode(readers)?)?)
            .await?;
        if let Some(recorded) = self.projection_access().await? {
            typed = typed.restrict(&recorded);
        }
        self.workspace.typed = Some(Arc::new(typed));
        self.start_events();
        Ok(())
    }
    pub(super) async fn install_delete_triggers(&self, typed: &Typed) -> Result<(), Error> {
        for record in rows(
            &self.conn,
            &format!(
                "SELECT name FROM sqlite_master WHERE type='trigger' AND name LIKE '{}%'",
                rocky_core::DELETE_TRIGGERS
            ),
        )
        .await?
        {
            let [SqlValue::Text(name)] = record.as_slice() else {
                return Err(Error::Corrupt("delete trigger".into()));
            };
            self.conn
                .execute(format!("DROP TRIGGER IF EXISTS {}", quoted(name)), ())
                .await?;
        }
        for sql in typed
            .rows
            .schema()
            .delete_triggers(|name| typed.installs(name))
        {
            self.conn.execute(sql, ()).await?;
        }
        Ok(())
    }
    pub(super) async fn reproject(&self, scope: ResourceScope) -> Result<(), Error> {
        if self.replayed_scope(scope) {
            return Ok(());
        }
        self.index_row_keys().await?;
        self.reindex_move_scope(scope).await?;
        self.require_declared(scope, "rebuild")
    }
    fn require_declared(&self, scope: ResourceScope, purpose: &str) -> Result<(), Error> {
        if self
            .workspace
            .typed
            .as_ref()
            .is_some_and(|typed| typed.declared_by_scope(scope).is_none())
        {
            return Err(Error::Corrupt(format!("missing typed scope for {purpose}")));
        }
        Ok(())
    }
}
