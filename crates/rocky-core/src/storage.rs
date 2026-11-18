use crate::{Application, Envelope, PrincipalId, Scope, SqlValue};

pub type Sql = std::borrow::Cow<'static, str>;

#[derive(Clone, Debug)]
pub struct ProjectionStatement {
    pub sql: Sql,
    pub values: Vec<SqlValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InstalledSchema {
    pub statements: Vec<Sql>,
    pub additive_indexes: Vec<Sql>,
    pub projection_query: Sql,
    pub checkpoint_tables: Vec<Sql>,
}
impl InstalledSchema {
    pub fn statements(&self) -> Vec<&str> {
        self.statements.iter().map(AsRef::as_ref).collect()
    }
}
pub trait ProjectionSchema {
    fn installed_schema(&self) -> InstalledSchema;
}
pub trait CompiledSchema {
    fn schema(&self) -> &'static [&'static str];
    fn additive_indexes(&self) -> &'static [&'static str] {
        &[]
    }
    fn projection_query(&self) -> &'static str;
    fn checkpoint_tables(&self) -> &'static [&'static str];
}
impl<T: CompiledSchema> ProjectionSchema for T {
    fn installed_schema(&self) -> InstalledSchema {
        let owned = |sql: &'static [&'static str]| sql.iter().map(|sql| Sql::Borrowed(sql));
        InstalledSchema {
            statements: owned(self.schema()).collect(),
            additive_indexes: owned(self.additive_indexes()).collect(),
            projection_query: Sql::Borrowed(self.projection_query()),
            checkpoint_tables: owned(self.checkpoint_tables()).collect(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ApplicationDescriptor {
    pub definition: &'static str,
    pub operations: crate::Versions,
    pub projection: u32,
}
pub struct AuthoredFollow {
    pub handle: crate::Commitment,
    pub original: crate::OperationId,
    pub intent: crate::Intent,
}

pub trait StoredApplication: Application + ProjectionSchema + Sized + Send + 'static {
    fn descriptor(&self) -> ApplicationDescriptor;
    fn manifest(&self) -> Option<crate::ApplicationManifest> {
        None
    }
    fn definition(&self) -> &'static str {
        self.descriptor().definition
    }
    type Row: Clone + Send + PartialEq + 'static;
    fn logical_schema(&self) -> Option<&crate::LogicalSchema> {
        None
    }
    fn following_queries(&self, _node: crate::NodeId) -> Vec<ProjectionStatement> {
        Vec::new()
    }
    fn following_intents(
        &self,
        _rows: Vec<Vec<SqlValue>>,
        _node: crate::NodeId,
    ) -> Result<Vec<AuthoredFollow>, Self::Error> {
        Ok(Vec::new())
    }
    fn projection_rows(&self, state: &Self::State) -> Vec<Vec<SqlValue>>;
    fn touched(&self, operation: &Envelope) -> Vec<ProjectionStatement>;
    fn window(&self, rows: Vec<Vec<SqlValue>>) -> Result<Self::State, Self::Error>;
    fn project(&self, state: &Self::State, operation: &Envelope) -> Vec<ProjectionStatement>;
    fn decode_rows(&self, rows: Vec<Vec<SqlValue>>) -> Result<Vec<Self::Row>, Self::Error>;
    fn select_rows(
        &self,
        rows: Vec<Self::Row>,
        context: crate::AdmissionContext,
        policy: &Self::Policy,
    ) -> Result<Vec<Self::Row>, Self::Error> {
        self.admit_read(context.scope, context.caller, policy)?;
        Ok(rows)
    }
    fn read_views(
        &self,
        context: crate::AdmissionContext,
        policy: &Self::Policy,
    ) -> Result<Vec<ReadView>, Self::Error> {
        self.admit_read(context.scope, context.caller, policy)?;
        Ok(Vec::new())
    }
    fn admit_read(
        &self,
        scope: Scope,
        caller: PrincipalId,
        policy: &Self::Policy,
    ) -> Result<(), Self::Error>;
}

#[derive(Clone, Debug, Default)]
pub struct ReadView {
    pub name: Sql,
    pub sql: Sql,
    pub values: Vec<SqlValue>,
    pub key: Vec<Sql>,
    pub not_null: Vec<Sql>,
}
pub fn columns(names: &[&'static str]) -> Vec<Sql> {
    names.iter().map(|name| Sql::Borrowed(name)).collect()
}

pub fn installed_definition<A: StoredApplication>(app: &A) -> crate::Commitment {
    let mut definition = app.definition().as_bytes().to_vec();
    if let Some(manifest) = app.manifest() {
        definition.extend(manifest.hash());
    }
    let schema = app.installed_schema();
    for sql in schema.statements.iter().chain([&schema.projection_query]) {
        definition.extend((sql.len() as u64).to_le_bytes());
        definition.extend(sql.as_bytes());
    }
    crate::commitment("rocky.installed-definition.v2", &definition)
}
