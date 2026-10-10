//! Concrete partition addresses and selector resolution (`docs/runtime/partitioned-scopes.md`).
use super::*;
use rocky_core::{PartitionSelection, PartitionTuple};

pub(super) const SCHEMA: &[&str] = &[
    "CREATE TABLE IF NOT EXISTS secure_partitions(scope BLOB PRIMARY KEY,tuple BLOB NOT NULL) WITHOUT ROWID",
];

impl<A: RowApplication> Inner<A> {
    pub(super) fn resolve_chain_at(
        &self,
        chain: &CredentialChain,
        manifest: Commitment,
        key: PublicKey,
        policy: &RevocationSet,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let (mut grants, selectors) = chain
            .verify_selectors(self.config.publisher, manifest, key, policy, time)
            .map_err(credential)?;
        for (scope, tuple) in &self.partitions {
            for selector in &selectors {
                if let Some(grant) = selector
                    .resolve(scope.domain, scope.table, tuple)
                    .map_err(credential)?
                {
                    grants.push(grant);
                }
            }
        }
        Ok(grants)
    }
    pub(super) fn resolve_chain_scopes_at(
        &self,
        chain: &CredentialChain,
        manifest: Commitment,
        key: PublicKey,
        policy: &RevocationSet,
        time: u64,
        scopes: &[ResourceScope],
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let (mut grants, selectors) = chain
            .verify_selectors(self.config.publisher, manifest, key, policy, time)
            .map_err(credential)?;
        grants.retain(|grant| scopes.contains(&grant.scope));
        for scope in scopes {
            if let Some(tuple) = self.partitions.get(scope) {
                for selector in &selectors {
                    if let Some(grant) = selector
                        .resolve(scope.domain, scope.table, tuple)
                        .map_err(credential)?
                    {
                        grants.push(grant);
                    }
                }
            }
        }
        Ok(grants)
    }
    pub(super) fn resolve_chain(
        &self,
        chain: &CredentialChain,
        key: PublicKey,
        policy: &RevocationSet,
        time: u64,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        self.resolve_chain_at(chain, self.manifest, key, policy, time)
    }
    pub(super) fn check_authenticated_partition(
        &self,
        scope: ResourceScope,
        tuple: Option<&PartitionTuple>,
    ) -> Result<(), Error> {
        if let Some(tuple) = tuple {
            return self.check_tuple(scope, tuple);
        }
        if self.partitioned(scope) {
            return Err(denied(
                "partitioned table requires an authenticated partition tuple",
            ));
        }
        Ok(())
    }
    /// Whether `scope` names a partitioned table; its rows live only in
    /// the scopes of its partitions, never in the table's own scope.
    pub(super) fn partitioned(&self, scope: ResourceScope) -> bool {
        self.typed.as_ref().is_some_and(|typed| {
            typed.rows.table(&scope.table).is_some_and(|table| {
                typed
                    .rows
                    .schema()
                    .table(&table.sql_name)
                    .is_some_and(|logical| !logical.partition_by.is_empty())
            })
        })
    }
    fn check_tuple(&self, scope: ResourceScope, tuple: &PartitionTuple) -> Result<(), Error> {
        if tuple
            .scope(scope.domain, scope.table)
            .map_err(|error| denied(&error.to_string()))?
            != scope
            || !self.tables.contains(&scope.table)
            || !self.domains.contains(&scope.domain.domain)
            || scope.domain.app != self.definition.app
        {
            return Err(denied("invalid partition address"));
        }
        if let Some(typed) = &self.typed {
            let table = typed
                .rows
                .table(&scope.table)
                .ok_or_else(|| denied("unknown partition table"))?;
            let logical = typed
                .rows
                .schema()
                .table(&table.sql_name)
                .ok_or_else(|| denied("unknown partition table"))?;
            let columns: BTreeSet<_> = logical
                .partition_by
                .iter()
                .filter_map(|name| logical.column(name))
                .map(|column| typed.rows.schema().field_id(logical, column))
                .collect();
            if columns.is_empty() || columns != tuple.values().keys().copied().collect() {
                return Err(denied("partition tuple differs from the schema"));
            }
            for name in &logical.partition_by {
                let column = logical
                    .column(name)
                    .ok_or_else(|| denied("unknown partition column"))?;
                let value = tuple
                    .values()
                    .get(&typed.rows.schema().field_id(logical, column))
                    .ok_or_else(|| denied("missing partition value"))?;
                let stored = rocky_core::PartitionValue::new(
                    rocky_core::KeyColumn {
                        affinity: column.affinity,
                        collation: column.collation,
                    },
                    value.value().map_err(|error| denied(&error.to_string()))?,
                )
                .map_err(|error| denied(&error.to_string()))?;
                if &stored != value {
                    return Err(denied("partition value is not canonical for the schema"));
                }
            }
        }
        Ok(())
    }
    pub(super) async fn load_partitions(&mut self) -> Result<(), Error> {
        for row in rows(&self.conn, "SELECT scope,tuple FROM secure_partitions").await? {
            let [SqlValue::Blob(scope), SqlValue::Blob(tuple)] = row.as_slice() else {
                return Err(Error::Corrupt("partition address".into()));
            };
            let scope = decode(scope)?;
            let tuple = decode(tuple)?;
            self.check_tuple(scope, &tuple)?;
            self.partitions.insert(scope, tuple);
        }
        Ok(())
    }
}
impl<A: RowApplication> EncryptedReplica<A> {
    pub(super) async fn discover_partition(&self, frame: &EncryptedFrame) -> Result<(), Error> {
        let operation = frame.operation()?;
        let Some(tuple) = operation.header.extensions.partition else {
            return Ok(());
        };
        let scope = operation.header.extensions.scope;
        {
            let inner = self.inner.lock().await;
            if inner.partitions.get(&scope) == Some(&tuple) {
                return Ok(());
            }
            inner.verify_frame(frame, now()?)?;
        }
        self.register_partition(scope, tuple).await
    }
    /// The prospective own-id partition. It is a log-derived candidate, not a reservation;
    /// the write transaction still allocates and verifies the identity.
    pub async fn next_partition_identity(
        &self,
        name: &str,
    ) -> Result<rocky_core::RowIdentity, Error> {
        let inner = self.inner.lock().await;
        let typed = inner
            .typed
            .as_ref()
            .ok_or_else(|| denied("no typed tables"))?;
        let table = typed.by_name(name).ok_or_else(|| denied("unknown table"))?;
        let logical = typed
            .rows
            .schema()
            .table(name)
            .filter(|table| table.partition_by == ["id"])
            .ok_or_else(|| denied("table must be partitioned by its own id"))?;
        let author = inner
            .row_authors
            .get(&table.table.id)
            .ok_or_else(|| denied("register a table pseudonym first"))?;
        let identity = rocky_core::RowIdentity::next(
            author.number,
            inner.highest_identity(table.scope, author.number).await?,
        )
        .map_err(|error| denied(&error.to_string()))?;
        let mut values = vec![SqlValue::Null; logical.columns.len()];
        let position = logical
            .columns
            .iter()
            .position(|column| column.sql_name.eq_ignore_ascii_case("id"))
            .ok_or_else(|| denied("missing id column"))?;
        values[position] = SqlValue::Integer(identity.integer());
        let tuple = typed
            .rows
            .partition(&table.table, &values)
            .map_err(|error| denied(&error))?
            .ok_or_else(|| denied("missing partition tuple"))?;
        let scope = tuple
            .scope(typed.domain, table.table.id)
            .map_err(|error| denied(&error.to_string()))?;
        let grants = author
            .credentials
            .verify_partition_author(
                inner.config.publisher,
                inner.manifest,
                author.key.public_key(),
                author.number,
                scope,
                &tuple,
                &inner.config.revocations,
                now()?,
            )
            .map_err(credential)?;
        if !grants
            .iter()
            .any(|grant| grant.rights.contains(&Right::Insert))
        {
            return Err(denied("own-id partition requires an insert grant"));
        }
        Ok(identity)
    }
    /// Registers an authorized concrete scope before subscribing or installing its key.
    pub async fn register_partition(
        &self,
        scope: ResourceScope,
        tuple: PartitionTuple,
    ) -> Result<(), Error> {
        let mut inner = self.inner.lock().await;
        inner.check_tuple(scope, &tuple)?;
        inner
            .config
            .credentials
            .verify_partition(
                inner.config.publisher,
                inner.manifest,
                inner.config.node.public_key(),
                scope,
                &tuple,
                &inner.config.revocations,
                now()?,
            )
            .map_err(credential)?;
        if inner.partitions.get(&scope) == Some(&tuple) {
            return Ok(());
        }
        if inner.partitions.len() >= 4096 {
            return Err(Error::Backpressure);
        }
        transaction!(inner, |inner| {
            inner
                .conn
                .execute(
                    "INSERT INTO secure_partitions VALUES(?,?)",
                    [encode(&scope)?, encode(&tuple)?],
                )
                .await?;
            Ok(())
        })?;
        inner.partitions.insert(scope, tuple);
        inner.bump_authority();
        Ok(())
    }
    pub async fn table_complete(&self, table: Commitment) -> Result<bool, Error> {
        self.inner.lock().await.table_complete(table).await
    }
}
impl<A: RowApplication> Inner<A> {
    pub(super) async fn table_complete(&self, table: Commitment) -> Result<bool, Error> {
        let (_, selectors) = self
            .config
            .credentials
            .verify_selectors(
                self.config.publisher,
                self.manifest,
                self.config.node.public_key(),
                &self.config.revocations,
                now()?,
            )
            .map_err(credential)?;
        let Some(typed) = &self.typed else {
            return Ok(false);
        };
        let Some(projected) = typed
            .tables
            .iter()
            .find(|projected| projected.table.id == table)
        else {
            return Ok(false);
        };
        let Some(logical) = typed.rows.schema().table(&projected.table.sql_name) else {
            return Ok(false);
        };
        let columns: BTreeSet<_> = logical
            .partition_by
            .iter()
            .filter_map(|name| logical.column(name))
            .map(|column| typed.rows.schema().field_id(logical, column))
            .collect();
        let covers = selectors.iter().any(|grant| {
            grant.domain == typed.domain
                && !columns.is_empty()
                && grant.selector.0.keys().copied().collect::<BTreeSet<_>>() == columns
                && grant.table == table
                && grant.rows == RowSelection::All
                && grant.rights.contains(&Right::Read)
                && grant
                    .selector
                    .0
                    .values()
                    .all(|selection| matches!(selection, PartitionSelection::Any))
        });
        if !covers || self.role().await?.role != ReplicaRole::Full {
            return Ok(false);
        }
        for scope in self.partitions.keys().filter(|scope| scope.table == table) {
            if !self.holds(*scope).await?
                || !self.can_read_key(*scope, &self.local_grants(now()?)?)
                || !self.config.keys.iter().any(|key| key.scope == *scope)
                || self.has_pending(*scope).await?
            {
                return Ok(false);
            }
        }
        Ok(true)
    }
}
impl<A: RowApplication> Inner<A> {
    pub(super) async fn partition_statement(
        &self,
        sql: &str,
        params: &[SqlValue],
        status: &SessionStatus,
    ) -> Result<String, Error> {
        let Some(typed) = &self.typed else {
            return Ok(sql.into());
        };
        let mut rules = BTreeMap::new();
        let support: Vec<&str> = typed
            .rows
            .schema()
            .support_tables()
            .into_iter()
            .map(|table| table.sql_name.as_str())
            .collect();
        for projected in &typed.tables {
            let logical = typed
                .rows
                .schema()
                .table(&projected.table.sql_name)
                .ok_or_else(|| denied("unknown table"))?;
            if logical.partition_by.is_empty() {
                // Puppets and the virtual row are never readable: a table
                // that may hold them reads its real rows only, and one
                // outside the session's readable scopes needs row_kind = 0.
                if support.contains(&logical.sql_name.as_str()) {
                    rules.insert(
                        logical.sql_name.to_ascii_lowercase(),
                        rocky_storage::partition_sql::TablePartitions {
                            columns: Vec::new(),
                            complete: status.readable.contains(&projected.scope),
                            unavailable: false,
                            held: Vec::new(),
                            support: true,
                        },
                    );
                }
                continue;
            }
            let mut held = Vec::new();
            let mut unavailable = false;
            let mut complete = self.table_complete(projected.table.id).await?;
            for (scope, tuple) in self.partitions.iter().filter(|(scope, _)| {
                scope.table == projected.table.id && scope.domain == typed.domain
            }) {
                if !status.readable.contains(scope) || !self.holds(*scope).await? {
                    complete = false;
                    continue;
                }
                if self.has_pending(*scope).await? {
                    unavailable = true;
                    continue;
                }
                held.push(
                    logical
                        .partition_by
                        .iter()
                        .map(|name| {
                            let column = logical
                                .column(name)
                                .ok_or_else(|| denied("unknown partition column"))?;
                            tuple
                                .values()
                                .get(&typed.rows.schema().field_id(logical, column))
                                .ok_or_else(|| denied("missing partition column"))?
                                .value()
                                .map_err(|error| denied(&error.to_string()))
                        })
                        .collect::<Result<Vec<_>, Error>>()?,
                );
            }
            rules.insert(
                logical.sql_name.to_ascii_lowercase(),
                rocky_storage::partition_sql::TablePartitions {
                    columns: logical
                        .partition_by
                        .iter()
                        .map(|name| {
                            let column = logical
                                .column(name)
                                .ok_or_else(|| denied("unknown partition column"))?;
                            Ok(rocky_storage::partition_sql::PartitionColumn {
                                name: name.clone(),
                                key: rocky_core::KeyColumn {
                                    affinity: column.affinity,
                                    collation: column.collation,
                                },
                            })
                        })
                        .collect::<Result<Vec<_>, Error>>()?,
                    complete,
                    unavailable,
                    held,
                    support: false,
                },
            );
        }
        if rules.is_empty() && typed.rows.schema().views.is_empty() {
            return Ok(sql.into());
        }
        rocky_storage::partition_sql::check_with_views(
            sql,
            params,
            &rules,
            &typed.rows.schema().views,
        )
        .map_err(|error| Error::Sql(crate::sql::SqlError::Unsupported(error)))
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Packs complete selected group partitions into bounded frontier exchanges.
    pub(crate) async fn frontier_batches(
        &self,
        scopes: &[ResourceScope],
        limit: usize,
    ) -> Result<Vec<Vec<ResourceScope>>, Error> {
        let inner = self.inner.lock().await;
        let mut groups = BTreeMap::<(rocky_core::Scope, Commitment), Vec<ResourceScope>>::new();
        for scope in scopes {
            let address =
                if let (Some(typed), Some(tuple)) = (&inner.typed, inner.partitions.get(scope)) {
                    let table = typed
                        .rows
                        .table(&scope.table)
                        .ok_or_else(|| denied("unknown partition table"))?;
                    let schema = typed.rows.schema();
                    let logical = schema
                        .table(&table.sql_name)
                        .ok_or_else(|| denied("unknown partition table"))?;
                    let values = logical
                        .partition_by
                        .iter()
                        .map(|name| {
                            let column = logical
                                .column(name)
                                .ok_or_else(|| denied("unknown partition column"))?;
                            tuple
                                .values()
                                .get(&schema.field_id(logical, column))
                                .ok_or_else(|| denied("missing partition value"))?
                                .value()
                                .map_err(|error| denied(&error.to_string()))
                        })
                        .collect::<Result<Vec<_>, Error>>()?;
                    schema
                        .group_address(scope.domain, &table.sql_name, &values)
                        .map_err(|error| denied(&error.to_string()))?
                        .commitment
                } else {
                    rocky_core::commitment("rocky.frontier-single-scope", &encode(scope)?)
                };
            groups
                .entry((scope.domain, address))
                .or_default()
                .push(*scope);
        }
        let mut batches = Vec::new();
        let mut batch = Vec::new();
        for group in groups.into_values() {
            if group.len() > limit {
                return Err(denied("partition group exceeds frontier batch limit"));
            }
            if batch.len() + group.len() > limit {
                batches.push(std::mem::take(&mut batch));
            }
            batch.extend(group);
        }
        if !batch.is_empty() {
            batches.push(batch);
        }
        Ok(batches)
    }
}

/// Custody of a partition without read: the grants of a keyless custody node.
fn custody_only(grants: &[rocky_core::ResourceGrant]) -> bool {
    grants
        .iter()
        .any(|grant| grant.rights.contains(&Right::Custody))
        && !grants
            .iter()
            .any(|grant| grant.rights.contains(&Right::Read))
}

impl<A: RowApplication> Inner<A> {
    /// `chain`'s grants on one concrete partition, under the policy of its generation.
    fn partition_grants(
        &self,
        chain: &CredentialChain,
        key: PublicKey,
        scope: ResourceScope,
        tuple: &PartitionTuple,
    ) -> Result<Vec<rocky_core::ResourceGrant>, Error> {
        let manifest = chain
            .0
            .first()
            .map_or(self.manifest, |root| root.capability.manifest);
        let revocations = self.revocations_for(manifest, &self.config.revocations)?;
        chain
            .verify_partition(
                self.config.publisher,
                manifest,
                key,
                scope,
                tuple,
                revocations,
                now()?,
            )
            .map_err(credential)
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The partition tuples of `scopes` that `peer` may hold only as keyless
    /// custody. Revealed to that peer alone, so it can register partitions
    /// created after it enrolled (`docs/decisions/0045-custody-partition-offers.md`).
    pub(crate) async fn custody_offers(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scopes: &[ResourceScope],
    ) -> Result<Vec<(ResourceScope, PartitionTuple)>, Error> {
        let inner = self.inner.lock().await;
        let manifest = credentials
            .0
            .first()
            .map_or(inner.manifest, |root| root.capability.manifest);
        // One chain verification per batch. A peer without custody, or of a
        // generation this replica cannot verify, gets no offer.
        let Ok(revocations) = inner.revocations_for(manifest, &inner.config.revocations) else {
            return Ok(Vec::new());
        };
        let Ok((_, selectors)) = credentials.verify_selectors(
            inner.config.publisher,
            manifest,
            peer,
            revocations,
            now()?,
        ) else {
            return Ok(Vec::new());
        };
        if !selectors
            .iter()
            .any(|selector| selector.rights.contains(&Right::Custody))
        {
            return Ok(Vec::new());
        }
        let mut offers = Vec::new();
        for scope in scopes {
            let Some(tuple) = inner.partitions.get(scope) else {
                continue;
            };
            let mut grants = Vec::new();
            for selector in &selectors {
                if let Ok(Some(grant)) = selector.resolve(scope.domain, scope.table, tuple) {
                    grants.push(grant);
                }
            }
            if custody_only(&grants) {
                offers.push((*scope, tuple.clone()));
            }
        }
        Ok(offers)
    }
    /// Registers offered partitions this node holds only as keyless custody,
    /// from a peer authorized to read or keep each of them. Other offers are
    /// refused; a known partition is unchanged.
    pub(crate) async fn accept_custody_offers(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scopes: &[ResourceScope],
        offers: &[(ResourceScope, PartitionTuple)],
    ) -> Result<(), Error> {
        if offers.len() > scopes.len() {
            return Err(denied("partition offers outside the frontier batch"));
        }
        let mut inner = self.inner.lock().await;
        let mut accepted = Vec::new();
        for (scope, tuple) in offers {
            if !scopes.contains(scope) {
                return Err(denied("partition offers outside the frontier batch"));
            }
            if inner.partitions.contains_key(scope) {
                continue;
            }
            inner.check_tuple(*scope, tuple)?;
            let own = inner.config.node.public_key();
            let held = inner.partition_grants(&inner.config.credentials, own, *scope, tuple)?;
            if !custody_only(&held) {
                return Err(denied(
                    "only a keyless custody node registers offered partitions",
                ));
            }
            let sender = inner.partition_grants(credentials, peer, *scope, tuple)?;
            if !sender.iter().any(|grant| {
                grant.rights.contains(&Right::Read) || grant.rights.contains(&Right::Custody)
            }) {
                return Err(denied(
                    "partition offer from a peer without read or custody",
                ));
            }
            accepted.push((*scope, tuple.clone()));
        }
        if accepted.is_empty() {
            return Ok(());
        }
        if inner.partitions.len() + accepted.len() > 4096 {
            return Err(Error::Backpressure);
        }
        transaction!(inner, |inner| {
            for (scope, tuple) in &accepted {
                inner
                    .conn
                    .execute(
                        "INSERT INTO secure_partitions VALUES(?,?)",
                        [encode(scope)?, encode(tuple)?],
                    )
                    .await?;
            }
            Ok(())
        })?;
        inner.partitions.extend(accepted);
        inner.bump_authority();
        Ok(())
    }
}
