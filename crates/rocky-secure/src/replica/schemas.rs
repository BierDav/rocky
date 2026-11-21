use super::headers::HeaderSource;
use super::*;
use rocky_core::{ApplicationManifest, Scope};
use rocky_credentials::TipGraph;

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_tip_schemas(manifest BLOB PRIMARY KEY,definition BLOB NOT NULL,record BLOB NOT NULL) WITHOUT ROWID",
    "CREATE TABLE IF NOT EXISTS secure_schema_switch(id INTEGER PRIMARY KEY CHECK(id=1),scope BLOB NOT NULL,bookmark TEXT NOT NULL,source BLOB NOT NULL,target BLOB NOT NULL)",
    "CREATE TABLE IF NOT EXISTS secure_pending_schema(id INTEGER PRIMARY KEY CHECK(id=1),scope BLOB NOT NULL,bookmark TEXT NOT NULL,tip BLOB NOT NULL,manifest BLOB NOT NULL,moved INTEGER NOT NULL CHECK(moved IN (0,1)))",
];

static IN_PLACE_SWITCH_CHANGES: std::sync::atomic::AtomicI64 =
    std::sync::atomic::AtomicI64::new(-1);
#[doc(hidden)]
pub fn last_in_place_switch_changes() -> i64 {
    IN_PLACE_SWITCH_CHANGES.load(std::sync::atomic::Ordering::SeqCst)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PendingSchema {
    pub scope: Scope,
    pub bookmark: String,
    pub tip: TipId,
    pub manifest: Commitment,
}

#[derive(Clone, Debug)]
pub struct InstalledTipSchema {
    pub tip: TipId,
    pub manifest: Commitment,
    pub record: Vec<u8>,
    pub continues: bool,
}

pub async fn installed_tip_schema(
    path: &Path,
    scope: Scope,
    publisher: PublicKey,
    from: Option<TipId>,
) -> Result<Option<InstalledTipSchema>, Error> {
    check_replica_format(path).await?;
    if !rocky_platform::fs::exists(path) {
        return Ok(None);
    }
    let conn = ScopedConnection::new(
        crate::sqlite::Connection::open_read_only(path)?,
        None,
        SCHEMA,
    );
    let has_table = !rows(
        &conn,
        "SELECT 1 FROM sqlite_master WHERE type='table' AND name='secure_followed_tip'",
    )
    .await?
    .is_empty();
    if !has_table {
        return Ok(None);
    }
    let records = rows(
        &conn,
        "SELECT scope,tip FROM secure_followed_tip WHERE id=1",
    )
    .await?;
    let Some([SqlValue::Blob(domain), SqlValue::Blob(tip)]) = records.first().map(Vec::as_slice)
    else {
        return Ok(None);
    };
    if decode::<Scope>(domain)? != scope {
        return Err(denied("installed tip scope mismatch"));
    }
    let tip: TipId = decode(tip)?;
    let roots = collect_rows(
        conn.query(
            "SELECT genesis FROM secure_tip_graphs WHERE scope=?",
            [domain.clone()],
        )
        .await?,
    )
    .await?;
    let Some([SqlValue::Blob(root)]) = roots.first().map(Vec::as_slice) else {
        return Err(Error::Corrupt("installed tip root".into()));
    };
    let root = checkpoint::row_of(root)?;
    let mut graph = rocky_credentials::TipGraph::new(scope, root);
    let mut statements = Vec::new();
    for row in rows(&conn, "SELECT statement FROM secure_tip_statements").await? {
        let [SqlValue::Blob(bytes)] = row.as_slice() else {
            return Err(Error::Corrupt("installed tip statement".into()));
        };
        let statement: TipStatement = decode(bytes)?;
        if statement.scope() == scope {
            statement.append(&mut graph);
            statements.push(statement);
        }
    }
    for statement in statements {
        statement.verify(publisher, &graph, None)?;
    }
    let manifest = graph.schema_any(tip).map_err(credential)?;
    let records = collect_rows(
        conn.query(
            "SELECT definition,record FROM secure_tip_schemas WHERE manifest=?",
            [manifest.to_vec()],
        )
        .await?,
    )
    .await?;
    let Some([SqlValue::Blob(definition), SqlValue::Blob(record)]) =
        records.first().map(Vec::as_slice)
    else {
        return Err(Error::Corrupt("installed tip schema".into()));
    };
    if ApplicationManifest::from_body(definition)
        .map_err(|error| Error::Corrupt(error.to_string()))?
        .hash()
        != manifest
    {
        return Err(Error::Corrupt("installed tip commitment".into()));
    }
    let continues =
        from.is_some_and(|from| from == tip || graph.continues(from, tip).unwrap_or(false));
    Ok(Some(InstalledTipSchema {
        tip,
        manifest,
        record: record.clone(),
        continues,
    }))
}

#[derive(Clone, Debug)]
pub(super) struct PathFields {
    pub tables: BTreeSet<Commitment>,
    pub fields: BTreeSet<Commitment>,
}
impl PathFields {
    pub(super) fn strips(&self, definition: &ApplicationManifest) -> bool {
        definition
            .entities
            .iter()
            .any(|table| !self.tables.contains(&table.id))
            || definition
                .fields
                .iter()
                .any(|field| !self.fields.contains(&field.id))
    }
    pub(super) fn empty() -> Self {
        Self {
            tables: BTreeSet::new(),
            fields: BTreeSet::new(),
        }
    }
    fn of(definition: &ApplicationManifest) -> Self {
        Self {
            tables: definition.entities.iter().map(|item| item.id).collect(),
            fields: definition.fields.iter().map(|item| item.id).collect(),
        }
    }
    fn intersect(&mut self, definition: &ApplicationManifest) {
        let next = Self::of(definition);
        self.tables.retain(|table| next.tables.contains(table));
        self.fields.retain(|field| next.fields.contains(field));
    }
    pub fn current(&self, write: &RowWrite) -> Option<RowWrite> {
        let kept = self.project(write)?;
        if kept.change.before.is_some()
            && kept.change.after.is_some()
            && kept.published_fields().is_empty()
        {
            return None;
        }
        Some(kept)
    }
    pub fn project(&self, write: &RowWrite) -> Option<RowWrite> {
        if !self.tables.contains(&write.change.scope.table) {
            return None;
        }
        let mut kept = write.clone();
        for fields in [&mut kept.change.before, &mut kept.change.after]
            .into_iter()
            .flatten()
        {
            fields.retain(|field, _| self.fields.contains(field));
        }
        kept.assigned.retain(|field| self.fields.contains(field));
        Some(kept)
    }
    pub fn strip(&self, table: Commitment, row: &mut Materialized) -> bool {
        let before = (row.fields.len(), row.stamps.len());
        row.fields
            .retain(|field, _| self.fields.contains(field) && self.tables.contains(&table));
        row.stamps
            .retain(|field, _| self.fields.contains(field) && self.tables.contains(&table));
        before != (row.fields.len(), row.stamps.len())
    }
}

impl<A: RowApplication> Inner<A> {
    pub(super) fn frame_schema_ready(&self, frame: &EncryptedFrame) -> Result<bool, Error> {
        if !self.engine_matches() {
            return Ok(false);
        }
        let header = frame.operation()?.header;
        let e = &header.extensions;
        let source = self
            .graph_for(e.scope.domain)?
            .schema_any(e.tip)
            .map_err(credential)?;
        if !self.catalog.contains_key(&source) {
            return Ok(false);
        }
        if matches!(
            self.frame_reach(frame)?,
            tips::FrameReach::Held | tips::FrameReach::Unfollowed
        ) {
            return Ok(true);
        }
        match self.path_fields(e.scope.domain, e.tip) {
            Ok(_) => Ok(true),
            Err(Error::Incompatible(_)) => Ok(false),
            Err(error) => Err(error),
        }
    }
    pub(super) async fn load_schemas(&mut self) -> Result<(), Error> {
        self.invalidate_context();
        let mut catalog = BTreeMap::new();
        for row in rows(
            &self.conn,
            "SELECT manifest,definition FROM secure_tip_schemas",
        )
        .await?
        {
            let [SqlValue::Blob(hash), SqlValue::Blob(body)] = row.as_slice() else {
                return Err(Error::Corrupt("tip schema record".into()));
            };
            let definition = ApplicationManifest::from_body(body)
                .map_err(|error| Error::Corrupt(error.to_string()))?;
            if definition.hash().as_slice() != hash
                || definition.publisher != *self.config.publisher.as_bytes()
                || definition.app != self.definition.app
            {
                return Err(Error::Corrupt("tip schema commitment".into()));
            }
            catalog.insert(definition.hash(), definition);
        }
        self.catalog = catalog;
        self.invalidate_context();
        Ok(())
    }
    pub(super) async fn store_schema(
        &self,
        definition: &ApplicationManifest,
        record: &[u8],
    ) -> Result<(), Error> {
        definition
            .validate()
            .map_err(|error| denied(&error.to_string()))?;
        if record.len() > rocky_core::MAX_DEFINITION_BYTES
            || definition.publisher != *self.config.publisher.as_bytes()
            || definition.app != self.definition.app
        {
            return Err(denied("invalid tip schema"));
        }
        self.conn.execute(
            "INSERT INTO secure_tip_schemas VALUES(?,?,?) ON CONFLICT(manifest) DO UPDATE SET record=CASE WHEN length(excluded.record)>0 THEN excluded.record ELSE record END",
            [definition.hash().to_vec(), definition.canonical_body(), record.to_vec()],
        ).await?;
        Ok(())
    }
    pub(super) async fn pending_intent(&self) -> Result<Option<(Scope, String, bool)>, Error> {
        match rows(
            &self.conn,
            "SELECT scope,bookmark,moved FROM secure_pending_schema WHERE id=1",
        )
        .await?
        .first()
        .map(Vec::as_slice)
        {
            Some(
                [
                    SqlValue::Blob(scope),
                    SqlValue::Text(bookmark),
                    SqlValue::Integer(moved),
                ],
            ) => Ok(Some((decode(scope)?, bookmark.clone(), *moved == 1))),
            None => Ok(None),
            _ => Err(Error::Corrupt("pending schema switch".into())),
        }
    }
    pub(super) fn on_workspace_path(&self, scope: Scope, tip: TipId) -> Result<bool, Error> {
        let graph = self.graph_for(scope)?;
        let mut cursor = self.workspace.tip;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            if cursor == tip {
                return Ok(true);
            }
            match Self::tip_parent(graph, cursor)? {
                Some((parent, _)) => cursor = parent,
                None => return Ok(false),
            }
        }
        Err(denied("cyclic tip path"))
    }
    pub(super) fn definition_at(&self, manifest: &Commitment) -> Option<&ApplicationManifest> {
        if *manifest == self.manifest {
            Some(&self.definition)
        } else {
            self.catalog.get(manifest)
        }
    }
    pub(super) fn path_fields(&self, scope: Scope, source: TipId) -> Result<PathFields, Error> {
        self.path_fields_to(scope, source, self.workspace.tip)
    }
    pub(super) fn path_fields_to(
        &self,
        scope: Scope,
        source: TipId,
        target: TipId,
    ) -> Result<PathFields, Error> {
        let key = (source, target);
        if let Some(fields) = self
            .path_fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&key)
            .cloned()
        {
            return Ok(fields);
        }
        let graph = self.graph_for(scope)?;
        let anchor = graph.late_anchor(source).map_err(credential)?;
        let mut path = Vec::new();
        let mut cursor = target;
        let mut visited = BTreeSet::new();
        while visited.insert(cursor) {
            path.push(cursor);
            if cursor == source || cursor == anchor {
                break;
            }
            let Some((parent, _)) = Self::tip_parent(graph, cursor)? else {
                return Err(denied("source tip is outside the workspace path"));
            };
            cursor = parent;
        }
        if path.last() != Some(&source) && path.last() != Some(&anchor) {
            return Err(denied("cyclic tip path"));
        }
        let mut fields = PathFields::of(&self.definition);
        if target != self.workspace.tip {
            let manifest = graph.schema_any(target).map_err(credential)?;
            if manifest != self.manifest {
                fields = PathFields::of(self.catalog.get(&manifest).ok_or_else(|| {
                    Error::Incompatible("a seal's schema is not installed".into())
                })?);
            }
        }
        for tip in path {
            let manifest = graph.schema_any(tip).map_err(credential)?;
            let definition = if manifest == self.manifest {
                &self.definition
            } else {
                self.catalog.get(&manifest).ok_or_else(|| {
                    Error::Incompatible("a schema on the followed path is not installed".into())
                })?
            };
            fields.intersect(definition);
        }
        self.path_fields
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(key, fields.clone());
        Ok(fields)
    }
    fn requirement_start(&self) -> Result<Option<Requirements>, Error> {
        let Some((scope, name)) = &self.workspace.followed else {
            return Ok(None);
        };
        let graph = self.graph_for(*scope)?;
        let Some(target) = graph.bookmark(name).map_err(credential)? else {
            return Ok(None);
        };
        let mut path = vec![target];
        let mut cursor = target;
        let mut visited = BTreeSet::from([target]);
        while let Some((parent, _)) = Self::tip_parent(graph, cursor)? {
            if !visited.insert(parent) {
                return Err(denied("cyclic tip path"));
            }
            path.push(parent);
            cursor = parent;
        }
        Ok(Some(Requirements {
            domain: *scope,
            graph: graph.clone(),
            path,
            retained: BTreeSet::new(),
            required: BTreeSet::new(),
        }))
    }
    pub(super) async fn schema_requirements(&self) -> Result<BTreeSet<(TipId, Commitment)>, Error> {
        let Some(mut requirements) = self.requirement_start()? else {
            return Ok(BTreeSet::new());
        };
        let domain = requirements.domain;
        requirements.visit(HeaderSource::Retained, self.domain_headers(domain).await?)?;
        let mut frames = Vec::new();
        for row in rows(&self.conn, "SELECT frame FROM secure_held_frames").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(Error::Corrupt("held schema reference".into()));
            };
            let frame: EncryptedFrame = decode(bytes)?;
            let header = &frame.operation_ref()?.header;
            if header.extensions.scope.domain == domain {
                frames.push(headers::ScannedHeader::new(header.clone()));
            }
        }
        requirements.visit(HeaderSource::HeldFrame, frames)?;
        self.finish_requirements(requirements, true).await
    }
    async fn finish_requirements(
        &self,
        requirements: Requirements,
        historical: bool,
    ) -> Result<BTreeSet<(TipId, Commitment)>, Error> {
        let Requirements {
            domain,
            graph,
            path,
            mut retained,
            mut required,
        } = requirements;
        let scopes = self.domain_scopes(domain).await?;
        if !scopes.is_empty() && path.contains(&self.workspace.tip) {
            retained.insert(self.workspace.tip);
        }
        let mut genesis_base = scopes.is_empty();
        for (_, id) in scopes {
            if let Some(seal) = self.base_seal(id).await? {
                retained.insert(graph.seal_by_id(seal).map_err(credential)?.statement.tip);
            } else {
                genesis_base = true;
            }
            if let Some(staging) = self.staging(id).await? {
                retained.insert(staging.signed.statement.tip);
            }
        }
        if genesis_base {
            retained.insert(graph.genesis());
        }
        for (seal, _) in self.kept_marks().await? {
            if let Ok(seal) = graph.seal_by_id(seal) {
                retained.insert(seal.statement.tip);
                if historical {
                    required.insert((
                        seal.statement.tip,
                        graph.schema_any(seal.statement.tip).map_err(credential)?,
                    ));
                }
            }
        }
        let oldest = path
            .iter()
            .rposition(|tip| retained.contains(tip))
            .unwrap_or(0);
        for tip in path.into_iter().take(oldest + 1) {
            required.insert((tip, graph.schema_any(tip).map_err(credential)?));
        }
        Ok(required)
    }
    pub(super) async fn required_schemas(&self) -> Result<BTreeSet<(TipId, Commitment)>, Error> {
        self.schema_requirements().await
    }
    pub(super) async fn missing_live_schemas(&self) -> Result<BTreeSet<Commitment>, Error> {
        let Some(mut requirements) = self.requirement_start()? else {
            return Ok(BTreeSet::new());
        };
        if requirements.path.iter().any(|tip| {
            requirements
                .graph
                .schema_any(*tip)
                .map_or(true, |manifest| !self.catalog.contains_key(&manifest))
        }) {
            let headers = self.domain_headers(requirements.domain).await?;
            requirements.visit(HeaderSource::Retained, headers)?;
        }
        Ok(self.missing_of(self.finish_requirements(requirements, false).await?))
    }
    fn missing_of(&self, required: BTreeSet<(TipId, Commitment)>) -> BTreeSet<Commitment> {
        required
            .into_iter()
            .map(|(_, manifest)| manifest)
            .filter(|manifest| !self.catalog.contains_key(manifest))
            .collect()
    }
}

async fn reinstall_switch_source<A: RowApplication>(
    inner: &mut Inner<A>,
    source: &mut generation::Definition<A>,
    typed: Option<Arc<typed::Typed>>,
) -> Result<(), Error> {
    source.swap(inner);
    inner.workspace.typed = typed;
    inner.load_schemas().await
}

struct Requirements {
    domain: rocky_core::Scope,
    graph: TipGraph,
    path: Vec<TipId>,
    retained: BTreeSet<TipId>,
    required: BTreeSet<(TipId, Commitment)>,
}
impl Requirements {
    fn visit(
        &mut self,
        source: HeaderSource,
        headers: Vec<headers::ScannedHeader>,
    ) -> Result<(), Error> {
        for header in headers {
            match source {
                HeaderSource::Retained if self.path.contains(&header.tip) => {
                    self.retained.insert(header.tip);
                }
                HeaderSource::Retained => {}
                HeaderSource::HeldFrame => {
                    let schema = self.graph.schema_any(header.tip).map_err(credential)?;
                    self.required.insert((header.tip, schema));
                }
            }
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub(super) async fn schema_requirements<T>(
        &self,
        historical: bool,
        act: impl AsyncFnOnce(&mut Inner<A>, BTreeSet<(TipId, Commitment)>) -> Result<T, Error>,
    ) -> Result<T, Error> {
        let domain = {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            inner.workspace.followed.as_ref().map(|(scope, _)| *scope)
        };
        let Some(domain) = domain else {
            let mut inner = self.inner.lock(crate::replica::Class::Background).await;
            if inner.workspace.followed.is_some() {
                return Err(Error::RetryExhausted);
            }
            return act(&mut inner, BTreeSet::new()).await;
        };
        self.fold_scan(
            domain,
            historical,
            |inner| match inner.requirement_start()? {
                Some(requirements) if requirements.domain == domain => Ok(Some(requirements)),
                Some(_) => Err(Error::RetryExhausted),
                None => Ok(None),
            },
            async |_, requirements, source, headers| match requirements {
                Some(requirements) => requirements.visit(source, headers),
                None => Ok(()),
            },
            async |inner, requirements| {
                let required = match requirements {
                    Some(requirements) => {
                        inner.finish_requirements(requirements, historical).await?
                    }
                    None => BTreeSet::new(),
                };
                act(inner, required).await
            },
        )
        .await
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn record_current_schema(&self, record: Vec<u8>) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        transaction!(inner, |inner| inner
            .store_schema(&inner.definition, &record)
            .await)?;
        inner.load_schemas().await
    }
    pub async fn required_schemas(&self) -> Result<BTreeSet<(TipId, Commitment)>, Error> {
        self.schema_requirements(true, async |_, required| Ok(required))
            .await
    }
    pub async fn missing_live_schemas(&self) -> Result<BTreeSet<Commitment>, Error> {
        self.schema_requirements(
            false,
            async |inner, required| Ok(inner.missing_of(required)),
        )
        .await
    }
    pub async fn install_tip_schema(
        &self,
        tip: TipId,
        definition: ApplicationManifest,
        record: Vec<u8>,
    ) -> Result<(), Error> {
        self.schema_requirements(true, async |inner, required| {
            if !required.contains(&(tip, definition.hash())) {
                return Err(denied("tip schema is not required"));
            }
            transaction!(inner, |inner| inner
                .store_schema(&definition, &record)
                .await)?;
            inner.load_schemas().await
        })
        .await?;
        self.settle().await
    }
    pub async fn tip_schema(&self, manifest: Commitment) -> Result<Option<Vec<u8>>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let records = collect_rows(
            inner
                .conn
                .query(
                    "SELECT record FROM secure_tip_schemas WHERE manifest=? AND length(record)>0",
                    [manifest.to_vec()],
                )
                .await?,
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            Some([SqlValue::Blob(record)]) => Ok(Some(record.clone())),
            None => Ok(None),
            _ => Err(Error::Corrupt("tip schema record".into())),
        }
    }
    pub async fn pending_schema(&self) -> Result<Option<PendingSchema>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let records = rows(
            &inner.conn,
            "SELECT scope,bookmark,tip,manifest FROM secure_pending_schema WHERE id=1",
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            Some(
                [
                    SqlValue::Blob(scope),
                    SqlValue::Text(bookmark),
                    SqlValue::Blob(tip),
                    SqlValue::Blob(manifest),
                ],
            ) => Ok(Some(PendingSchema {
                scope: decode(scope)?,
                bookmark: bookmark.clone(),
                tip: decode(tip)?,
                manifest: checkpoint::row_of(manifest)?,
            })),
            None => Ok(None),
            _ => Err(Error::Corrupt("pending schema switch".into())),
        }
    }
}

type ProjectionTables = (
    rocky_core::LogicalSchema,
    Scope,
    BTreeMap<String, Commitment>,
);

pub struct SchemaConfig {
    pub security: SecureConfig,
    pub tables: Option<ProjectionTables>,
}

impl<A: RowApplication> Inner<A> {
    async fn switch_schema_now(
        &mut self,
        tip: TipId,
        app: A,
        config: SchemaConfig,
    ) -> Result<bool, Error> {
        IN_PLACE_SWITCH_CHANGES.store(-1, std::sync::atomic::Ordering::SeqCst);
        let inner = self;
        let manifest = app.manifest();
        if manifest.engine != inner.engine {
            return Err(Error::Incompatible(
                super::generation::ENGINE_UPGRADE_REQUIRED.into(),
            ));
        }
        let (scope, name) = inner
            .workspace
            .followed
            .clone()
            .ok_or_else(|| denied("schema switching requires following a bookmark"))?;
        if inner.schema_switching().await? {
            transaction!(inner, |inner| inner.reconcile_staged_switch().await)?;
        }
        let staged = inner.schema_switch_intent().await?;
        if let Some((staged_scope, staged_name, staged_source, staged_target)) = &staged
            && (*staged_scope != scope
                || *staged_name != name
                || *staged_source != inner.workspace.tip
                || *staged_target != tip)
        {
            return Err(denied(
                "a different schema switch is staged; withdraw it before switching",
            ));
        }
        inner.authenticate_switch(
            scope,
            &name,
            tip,
            &manifest,
            &config.security,
            staged.is_some(),
        )?;
        if inner.manifest == manifest.hash() && inner.workspace.tip == tip {
            return Ok(false);
        }
        let source = inner.workspace.tip;
        let mut definition = inner
            .switch_definition(tip, app, config.security, config.tables.as_ref())
            .await?;
        let old_typed = inner.workspace.typed.clone();
        definition.swap(inner);
        inner.invalidate_context();
        let changes_before = inner.conn.connection().total_changes()?;
        let staged = transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT INTO secure_schema_switch VALUES(1,?,?,?,?) ON CONFLICT(id) DO NOTHING",
                    bindings(vec![
                        SqlValue::Blob(encode(&scope)?),
                        SqlValue::Text(name.clone()),
                        SqlValue::Blob(encode(&source)?),
                        SqlValue::Blob(encode(&tip)?),
                    ]),
                )
                .await?;
            inner.store_schema(&inner.definition, &[]).await?;
            inner.hold_removed(scope, tip).await?;
            let fields = inner.durable_switch_fields(scope, source, tip).await?;
            let stripped = inner.strip_rows(scope, source, &fields).await?;
            if let Some((schema, domain, readers)) = &config.tables {
                let expected = schema.manifest_at(
                    &inner
                        .definition
                        .domains
                        .iter()
                        .map(|rule| rule.domain)
                        .collect::<Vec<_>>(),
                    inner.definition.schema,
                    inner.definition.engine,
                );
                if expected.hash() != inner.manifest {
                    return Err(denied("projection differs from tip schema"));
                }
                if !stripped
                    && inner
                        .switch_in_place(schema.clone(), *domain, readers)
                        .await?
                {
                    inner.finish_schema_switch().await?;
                    return Ok(true);
                }
                inner
                    .record_plan(schema, *domain, readers, super::migrate::PlanCause::Switch)
                    .await?;
                return Ok(false);
            }
            if inner.workspace.typed.is_some() {
                return Err(denied("schema switch must supply its SQL projection"));
            }
            inner.finish_schema_switch().await?;
            Ok(false)
        });
        let in_place = match staged {
            Ok(in_place) => in_place,
            Err(error) => {
                reinstall_switch_source(inner, &mut definition, old_typed).await?;
                return Err(error);
            }
        };
        if in_place {
            let changes = inner
                .conn
                .connection()
                .total_changes()?
                .saturating_sub(changes_before);
            IN_PLACE_SWITCH_CHANGES.store(
                i64::try_from(changes).unwrap_or(i64::MAX),
                std::sync::atomic::Ordering::SeqCst,
            );
        }
        if !in_place {
            inner.load_schemas().await?;
            let finished = inner.finish_plan().await;
            if !matches!(finished, Ok(Ok(()))) {
                if inner.schema_switching().await? {
                    reinstall_switch_source(inner, &mut definition, old_typed).await?;
                    if matches!(finished, Ok(Err(_))) {
                        transaction!(inner, |inner| {
                            inner.withdraw_schema_switch(scope, &name).await
                        })?;
                    }
                }
                finished?.map_err(|broken| broken.refuses(Rebuilding::SchemaProjection))?;
            }
        }
        inner.load_schemas().await?;
        inner.load_partitions().await?;
        inner.start_events();
        Ok(true)
    }
    fn authenticate_switch(
        &self,
        scope: Scope,
        name: &str,
        tip: TipId,
        manifest: &ApplicationManifest,
        security: &SecureConfig,
        staged: bool,
    ) -> Result<(), Error> {
        let graph = self.graph_for(scope)?;
        let followed = match graph.bookmark(name).map_err(credential)? {
            Some(followed) if followed == tip => true,
            Some(followed) if staged => graph.continues(tip, followed).unwrap_or(false),
            _ => false,
        };
        if !followed
            || graph.schema(tip).map_err(credential)? != manifest.hash()
            || security
                .manifest
                .verify_historic(security.publisher)
                .map_err(credential)?
                .hash()
                != graph.genesis_schema
        {
            return Err(denied("schema does not authenticate the followed tip"));
        }
        Ok(())
    }
    async fn switch_definition(
        &self,
        tip: TipId,
        app: A,
        mut security: SecureConfig,
        tables: Option<&ProjectionTables>,
    ) -> Result<generation::Definition<A>, Error> {
        let manifest = app.manifest();
        if self.config.publisher != security.publisher
            || self.config.node.public_key() != security.node.public_key()
            || self.config.credentials.principal() != security.credentials.principal()
            || self.definition.domains != manifest.domains
            || self.definition.app != manifest.app
            || self.definition.operations != manifest.operations
        {
            return Err(denied(
                "schema switch changed domain identity or operations",
            ));
        }
        let missing = self.missing_live_schemas().await?;
        if !missing.is_empty() {
            return Err(Error::Incompatible(format!(
                "followed path schemas missing: {missing:?}"
            )));
        }
        security
            .credentials
            .verify_database(
                self.config
                    .credentials
                    .database_context(self.config.publisher)
                    .map_err(credential)?,
            )
            .map_err(credential)?;
        let families = match tables {
            Some((schema, domain, readers)) => {
                typed::Typed::new(schema.clone(), *domain, readers)?.families
            }
            None => config_families(&security, &manifest)?,
        };
        security
            .revocations
            .pin(&security.manifest, security.publisher)
            .map_err(credential)?;
        check_config_with_families(&security, &manifest, &families)?;
        let (current, mut history) = keys::split(std::mem::take(&mut security.keys));
        security.keys = current;
        for key in &self.config.keys {
            if manifest
                .entities
                .iter()
                .any(|table| table.id == key.scope.table)
                && !security.keys.iter().any(|held| held.scope == key.scope)
            {
                security.keys.push(key.clone());
            }
        }
        for key in self.opening_keys() {
            if !security
                .keys
                .iter()
                .chain(&history)
                .any(|held| held.scope == key.scope && held.generation == key.generation)
            {
                history.push(key.clone());
            }
        }
        let permission_fields = app.permission_fields().into_iter().collect();
        Ok(generation::Definition {
            app: Arc::new(app),
            permission_fields,
            chain: Chain::new(security.credentials.clone())?,
            node_author: self.node_author.clone(),
            families,
            config: security,
            history,
            manifest: manifest.hash(),
            tip,
            tables: manifest.entities.iter().map(|item| item.id).collect(),
            fields: manifest.fields.iter().map(|item| item.id).collect(),
            domains: manifest.domains.iter().map(|rule| rule.domain).collect(),
            definition: manifest,
        })
    }
    pub(in crate::replica) async fn schema_switch_intent(
        &self,
    ) -> Result<Option<(Scope, String, TipId, TipId)>, Error> {
        let records = rows(
            &self.conn,
            "SELECT scope,bookmark,source,target FROM secure_schema_switch WHERE id=1",
        )
        .await?;
        match records.first().map(Vec::as_slice) {
            Some(
                [
                    SqlValue::Blob(scope),
                    SqlValue::Text(bookmark),
                    SqlValue::Blob(source),
                    SqlValue::Blob(target),
                ],
            ) => Ok(Some((
                decode(scope)?,
                bookmark.clone(),
                decode(source)?,
                decode(target)?,
            ))),
            None => Ok(None),
            _ => Err(Error::Corrupt("schema switch intent".into())),
        }
    }
    async fn stored_definition(&self, manifest: Commitment) -> Result<ApplicationManifest, Error> {
        let records = collect_rows(
            self.conn
                .query(
                    "SELECT definition FROM secure_tip_schemas WHERE manifest=?",
                    [manifest.to_vec()],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) else {
            return Err(denied("switch schema missing"));
        };
        ApplicationManifest::from_body(bytes).map_err(|error| Error::Corrupt(error.to_string()))
    }
    async fn durable_switch_fields(
        &self,
        scope: Scope,
        source: TipId,
        target: TipId,
    ) -> Result<PathFields, Error> {
        let graph = self.graph_for(scope)?;
        let target_definition = self
            .stored_definition(graph.schema_any(target).map_err(credential)?)
            .await?;
        let mut fields = PathFields::of(&target_definition);
        let source_continues_to_target =
            source == target || graph.continues(source, target).unwrap_or(false);
        if source_continues_to_target {
            let mut cursor = target;
            while cursor != source {
                cursor = Self::tip_parent(graph, cursor)?
                    .ok_or_else(|| denied("switch source is not an ancestor"))?
                    .0;
                fields.intersect(
                    &self
                        .stored_definition(graph.schema_any(cursor).map_err(credential)?)
                        .await?,
                );
            }
        }
        Ok(fields)
    }
    pub(in crate::replica) async fn staged_row_fields(
        &self,
        domain: Scope,
    ) -> Result<Option<PathFields>, Error> {
        let Some((scope, _, source, target)) = self.schema_switch_intent().await? else {
            return Ok(None);
        };
        if scope != domain {
            return Ok(None);
        }
        let fields = self.durable_switch_fields(scope, source, target).await?;
        let before = self
            .stored_definition(
                self.graph_for(scope)?
                    .schema_any(source)
                    .map_err(credential)?,
            )
            .await?;
        Ok(fields.strips(&before).then_some(fields))
    }
    pub(super) async fn follow_resolved(&mut self, scope: Scope, name: &str) -> Result<(), Error> {
        if let Some((staged, bookmark, _, _)) = self.schema_switch_intent().await?
            && staged == scope
            && bookmark == name
        {
            return self.reconcile_staged_switch().await;
        }
        if let Some(target) = self.graph_for(scope)?.bookmark(name).map_err(credential)? {
            self.switch_tip(scope, name, target).await?;
        }
        Ok(())
    }
    pub(super) async fn reconcile_staged_switch(&mut self) -> Result<(), Error> {
        let Some((scope, bookmark, _, target)) = self.schema_switch_intent().await? else {
            return Ok(());
        };
        if self.own_staged_target(target).await? {
            return Ok(());
        }
        let graph = self.graph_for(scope)?;
        let followed = graph.bookmark(&bookmark).map_err(credential)?;
        let continues = graph.schema(target).is_ok()
            && followed.is_some_and(|followed| {
                followed == target || graph.continues(target, followed).unwrap_or(false)
            });
        if continues {
            return Ok(());
        }
        self.withdraw_schema_switch(scope, &bookmark).await
    }
    async fn own_staged_target(&self, target: TipId) -> Result<bool, Error> {
        for record in rows(
            &self.conn,
            "SELECT stage FROM secure_staged_schema WHERE state='staged'",
        )
        .await?
        {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("staged schema".into()));
            };
            if decode::<super::staging::SchemaStage>(bytes)?.target == target {
                return Ok(true);
            }
        }
        Ok(false)
    }
    pub(super) async fn withdraw_schema_switch(
        &mut self,
        scope: Scope,
        bookmark: &str,
    ) -> Result<(), Error> {
        let Some((_, _, source, _)) = self.schema_switch_intent().await? else {
            return Ok(());
        };
        if self.workspace.tip != source
            || self
                .graph_for(scope)?
                .schema_any(source)
                .map_err(credential)?
                != self.manifest
        {
            return Err(denied(
                "withdrawing a staged schema switch needs its source schema installed",
            ));
        }
        self.restore_source(scope, bookmark).await
    }
    async fn restore_source(&mut self, scope: Scope, bookmark: &str) -> Result<(), Error> {
        self.invalidate_context();
        self.conn
            .execute("DELETE FROM secure_schema_switch WHERE id=1", ())
            .await?;
        self.discard_plan().await?;
        let mut replayed = Vec::new();
        for record in rows(
            &self.conn,
            "SELECT id,scope FROM secure_scope_state ORDER BY id",
        )
        .await?
        {
            let [SqlValue::Integer(id), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope state".into()));
            };
            let resource: ResourceScope = decode(bytes)?;
            if resource.domain != scope {
                continue;
            }
            if self.replayed_scope(resource) {
                replayed.push(*id);
            } else {
                self.restore_rows(resource, i64::MAX).await?;
                self.reproject(resource).await?;
            }
        }
        if !replayed.is_empty()
            && let Err(broken) = self.replay_from_frames(now()?).await?
        {
            self.rederive_later(&replayed, broken).await?;
        }
        self.conn
            .execute(
                "UPDATE secure_metadata SET revision=revision+1 WHERE id=1",
                (),
            )
            .await?;
        match self
            .graph_for(scope)?
            .bookmark(bookmark)
            .map_err(credential)?
        {
            Some(followed) => self.switch_tip(scope, bookmark, followed).await,
            None => {
                self.conn
                    .execute("DELETE FROM secure_pending_schema WHERE id=1", ())
                    .await?;
                Ok(())
            }
        }
    }
    async fn withdraw_stage(
        &mut self,
        stage: &super::staging::SchemaStage,
        app: A,
        config: SchemaConfig,
    ) -> Result<(), Error> {
        let (graphs, tip) = (self.graphs.clone(), self.workspace.tip);
        let author = self.node_author.clone();
        let nodes = self.line_nodes.clone();
        let followed = self.workspace.followed.clone();
        let ids: BTreeSet<Commitment> = stage
            .statements
            .iter()
            .map(TipStatement::id)
            .collect::<Result<_, _>>()?;
        self.invalidate_context();
        if let Some(graph) = self.graphs.get_mut(&stage.scope) {
            let graph = Arc::make_mut(graph);
            graph
                .seals
                .retain(|seal| seal.id().is_ok_and(|id| !ids.contains(&id)));
            graph
                .openings
                .retain(|opening| opening.id().is_ok_and(|id| !ids.contains(&id)));
            graph
                .moves
                .retain(|movement| movement.id().is_ok_and(|id| !ids.contains(&id)));
        }
        self.workspace.tip = stage.source;
        self.invalidate_context();
        match self.commit_withdrawal(stage, app, config).await {
            Ok(reinstalled) => {
                let reloaded = if reinstalled {
                    self.reload_withdrawn_source().await
                } else {
                    Ok(())
                };
                self.publish_revision().await?;
                reloaded
            }
            Err(error) => {
                self.graphs = graphs;
                self.workspace.tip = tip;
                self.node_author = author;
                self.line_nodes = nodes;
                self.workspace.followed = followed;
                self.invalidate_context();
                Err(error)
            }
        }
    }
    async fn commit_withdrawal(
        &mut self,
        stage: &super::staging::SchemaStage,
        app: A,
        config: SchemaConfig,
    ) -> Result<bool, Error> {
        let source = self
            .graph_for(stage.scope)?
            .schema_any(stage.source)
            .map_err(credential)?;
        let manifest = app.manifest();
        if manifest.hash() != source {
            return Err(denied(
                "withdrawing a staged schema switch needs its source schema installed",
            ));
        }
        if self.workspace.typed.is_some() && config.tables.is_none() {
            return Err(denied("schema switch must supply its SQL projection"));
        }
        let mut reinstalled = None;
        if self.manifest != source {
            self.authenticate_switch(
                stage.scope,
                &stage.bookmark,
                stage.source,
                &manifest,
                &config.security,
                false,
            )?;
            let mut definition = self
                .switch_definition(stage.source, app, config.security, config.tables.as_ref())
                .await?;
            let staged = self.workspace.typed.clone();
            definition.swap(self);
            reinstalled = Some((definition, staged));
        }
        let restored = transaction!(self, |inner| {
            inner.discard_schema_stage(stage).await?;
            inner.discard_tip_writes(stage.target).await?;
            inner
                .rebind_node_tips(stage.scope, stage.target, stage.source)
                .await?;
            if let Some((_, staged)) = &reinstalled
                && let Some((schema, domain, readers)) = &config.tables
            {
                inner
                    .reinstall_projection(staged.as_deref(), schema, *domain, readers)
                    .await?;
            }
            inner.forget_undeclared_scopes(stage.scope).await?;
            inner.restore_source(stage.scope, &stage.bookmark).await?;
            inner.load_tip_graphs().await
        });
        match (restored, reinstalled) {
            (Ok(()), reinstalled) => Ok(reinstalled.is_some()),
            (Err(error), Some((mut definition, staged))) => {
                self.abandon_switch_history();
                reinstall_switch_source(self, &mut definition, staged).await?;
                Err(error)
            }
            (Err(error), None) => Err(error),
        }
    }
    async fn reload_withdrawn_source(&mut self) -> Result<(), Error> {
        let loaded = self.load_withdrawn_schemas().await;
        self.start_events();
        self.bump_authority();
        loaded
    }
    async fn load_withdrawn_schemas(&mut self) -> Result<(), Error> {
        #[cfg(test)]
        if std::mem::take(&mut self.withdrawal_reload_fault) {
            return Err(Error::InjectedFailure);
        }
        self.load_schemas().await?;
        self.load_partitions().await
    }
    async fn reinstall_projection(
        &mut self,
        staged: Option<&typed::Typed>,
        schema: &rocky_core::LogicalSchema,
        domain: Scope,
        readers: &BTreeMap<String, Commitment>,
    ) -> Result<(), Error> {
        let installed = self
            .installed_typed()
            .await?
            .ok_or_else(|| Error::Corrupt("a staged schema without typed tables".into()))?
            .0;
        let projected = Arc::new(
            self.authorized_projection(typed::Typed::new(schema.clone(), domain, readers)?)
                .await?,
        );
        self.workspace.typed = Some(projected.clone());
        self.install_projection(
            &installed,
            schema,
            staged,
            &projected,
            Some(&PathFields::empty()),
        )
        .await?
        .map_err(|broken| broken.refuses(Rebuilding::SchemaProjection))?;
        if let Some(staged) = staged {
            self.drop_unprojected_history(staged, &projected).await?;
        }
        self.store_typed(schema, domain, readers).await?;
        self.cause(crate::observe::Cause::Rebuild);
        Ok(())
    }
    pub(super) async fn schema_switching(&self) -> Result<bool, Error> {
        Ok(
            !rows(&self.conn, "SELECT 1 FROM secure_schema_switch WHERE id=1")
                .await?
                .is_empty(),
        )
    }
    async fn strip_rows(
        &mut self,
        scope: Scope,
        source: TipId,
        fields: &PathFields,
    ) -> Result<bool, Error> {
        let before = self
            .stored_definition(
                self.graph_for(scope)?
                    .schema_any(source)
                    .map_err(credential)?,
            )
            .await?;
        if !fields.strips(&before) {
            return Ok(false);
        }
        let mut stored = Vec::new();
        for record in rows(
            &self.conn,
            "SELECT id,scope FROM secure_scope_state ORDER BY id",
        )
        .await?
        {
            let [SqlValue::Integer(id), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope state".into()));
            };
            let resource: ResourceScope = decode(bytes)?;
            if resource.domain == scope {
                stored.push((resource, *id));
            }
        }
        for (resource, id) in stored {
            let mut after = Vec::new();
            loop {
                let records = collect_rows(self.conn.query("SELECT row,fields FROM secure_row_records WHERE scope=? AND row>? ORDER BY row LIMIT 256", [encode(&resource)?, after.clone()]).await?).await?;
                for row in &records {
                    let [SqlValue::Blob(key), SqlValue::Blob(bytes)] = row.as_slice() else {
                        return Err(Error::Corrupt("schema switch row".into()));
                    };
                    after = key.clone();
                    match self
                        .strip_stored_record(fields, resource.table, bytes)
                        .await?
                    {
                        None => {
                            self.conn
                                .execute(
                                    "DELETE FROM secure_row_records WHERE scope=? AND row=?",
                                    [encode(&resource)?, key.clone()],
                                )
                                .await?;
                        }
                        Some(stripped) if stripped != *bytes => {
                            self.conn
                                .execute(
                                    "UPDATE secure_row_records SET fields=? WHERE scope=? AND row=?",
                                    [stripped, encode(&resource)?, key.clone()],
                                )
                                .await?;
                        }
                        Some(_) => {}
                    }
                }
                if records.len() < 256 {
                    break;
                }
            }
            self.recount(id, resource).await?;
        }
        Ok(true)
    }
    pub(super) async fn finish_schema_switch(&mut self) -> Result<(), Error> {
        let records = rows(
            &self.conn,
            "SELECT scope,bookmark,source,target FROM secure_schema_switch WHERE id=1",
        )
        .await?;
        let Some(
            [
                SqlValue::Blob(scope),
                SqlValue::Text(bookmark),
                SqlValue::Blob(source),
                SqlValue::Blob(target),
            ],
        ) = records.first().map(Vec::as_slice)
        else {
            if records.is_empty() {
                return Ok(());
            }
            return Err(Error::Corrupt("schema switch intent".into()));
        };
        let scope: Scope = decode(scope)?;
        let source: TipId = decode(source)?;
        let target: TipId = decode(target)?;
        let graph = self.graph_for(scope)?;
        let followed = graph.bookmark(bookmark).map_err(credential)?;
        if !followed.is_some_and(|followed| {
            followed == target || graph.continues(target, followed).unwrap_or(false)
        }) || graph.schema(target).map_err(credential)? != self.manifest
        {
            return Err(denied("schema switch target no longer resolves"));
        }
        for record in rows(&self.conn, "SELECT id,scope FROM secure_scope_state").await? {
            let [SqlValue::Integer(id), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope state".into()));
            };
            let resource: ResourceScope = decode(bytes)?;
            if resource.domain == scope && !self.tables.contains(&resource.table) {
                self.clear_seal_anchor(*id).await?;
            }
        }
        self.conn
            .execute("DELETE FROM secure_schema_switch WHERE id=1", ())
            .await?;
        self.invalidate_context();
        self.workspace.tip = source;
        self.switch_tip(scope, bookmark, target).await?;
        if let Some(followed) = followed
            && followed != target
        {
            self.switch_tip(scope, bookmark, followed).await?;
        }
        self.rebuild_voided_in_transaction().await?;
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn switch_schema(
        &self,
        tip: TipId,
        app: A,
        config: SchemaConfig,
    ) -> Result<bool, Error> {
        let switched = self
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .switch_schema_now(tip, app, config)
            .await?;
        self.settle().await?;
        Ok(switched)
    }
    pub async fn withdraw_staged_schema(
        &self,
        publication: Commitment,
        app: A,
        config: SchemaConfig,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let records = collect_rows(
            inner
                .conn
                .query(
                    "SELECT stage FROM secure_staged_schema WHERE publication=? AND state='staged'",
                    [publication.to_vec()],
                )
                .await?,
        )
        .await?;
        let Some([SqlValue::Blob(bytes)]) = records.first().map(Vec::as_slice) else {
            return Err(denied(
                "only an uncommitted staged schema publication withdraws",
            ));
        };
        let stage: super::staging::SchemaStage = decode(bytes)?;
        if inner.publication_cursor(publication).await?.is_some() {
            return Err(denied(
                "a partially authored schema publication never withdraws",
            ));
        }
        inner.withdraw_stage(&stage, app, config).await
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    #[cfg(test)]
    pub(crate) async fn fail_next_withdrawal_reload(&self) {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .withdrawal_reload_fault = true;
    }
    pub async fn drop_unrequired_schemas(&self) -> Result<Vec<Commitment>, Error> {
        self.schema_requirements(true, async |inner, required| {
            inner.drop_unrequired_schemas(required).await
        })
        .await
    }
}

impl<A: RowApplication> Inner<A> {
    async fn drop_unrequired_schemas(
        &mut self,
        required: BTreeSet<(TipId, Commitment)>,
    ) -> Result<Vec<Commitment>, Error> {
        let inner = self;
        let mut kept: BTreeSet<_> = required.into_iter().map(|(_, manifest)| manifest).collect();
        kept.insert(inner.manifest);
        kept.insert(
            inner
                .config
                .manifest
                .verify_historic(inner.config.publisher)
                .map_err(credential)?
                .hash(),
        );
        for row in rows(&inner.conn, "SELECT o.header FROM secure_operations o WHERE EXISTS(SELECT 1 FROM secure_publication_frames p WHERE p.hash=o.hash) OR EXISTS(SELECT 1 FROM secure_migration_frames m WHERE m.hash=o.hash)").await? {
            let [SqlValue::Blob(bytes)] = row.as_slice() else { return Err(Error::Corrupt("publication schema reference".into())) };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            kept.insert(inner.manifest_of_tip(header.extensions.tip)?);
        }
        let dropped: Vec<_> = inner
            .catalog
            .keys()
            .filter(|manifest| !kept.contains(*manifest))
            .copied()
            .collect();
        transaction!(inner, |inner| {
            for manifest in &dropped {
                inner
                    .conn
                    .execute(
                        "DELETE FROM secure_tip_schemas WHERE manifest=?",
                        [manifest.to_vec()],
                    )
                    .await?;
            }
            Ok(())
        })?;
        inner.load_schemas().await?;
        Ok(dropped)
    }
}

#[cfg(test)]
impl<A: RowApplication> EncryptedReplica<A> {
    pub(crate) async fn schema_context(&self) -> Vec<u8> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let author = |author: &identity::NodeAuthor| {
            (
                author.key.public_key(),
                author.credentials.clone(),
                author.writer,
            )
        };
        encode(&(
            inner.manifest,
            inner.workspace.tip,
            &inner.workspace.followed,
            inner.node_author.as_ref().map(author),
            inner
                .line_nodes
                .iter()
                .map(|(tip, node)| (*tip, author(node)))
                .collect::<Vec<_>>(),
            inner.config.node.public_key(),
            &inner.config.credentials,
        ))
        .unwrap()
    }
}
