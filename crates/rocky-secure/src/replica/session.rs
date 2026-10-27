//! Authorized sessions: a principal's bounded view of a secure replica and
//! its transactions, which commit as bundles. See
//! `docs/runtime/secure-sessions.md`.
use super::*;

/// What a session is bound to. Authentication and enrollment supply these
/// when the replica opens; no call argument can claim another principal.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionStatus {
    /// The user the node's credential chain delegates from: the login identity.
    pub principal: PublicKey,
    /// The device signing identity that authors every operation.
    pub node: PublicKey,
    pub manifest: Commitment,
    /// The digest of the live revocations the credentials were verified under.
    pub revocations: Commitment,
    /// How many live revocation statements the replica holds.
    pub revoked: usize,
    /// The earliest expiry in the credential chain, in seconds.
    pub expires: u64,
    /// The resource set the session was opened with.
    pub scopes: BTreeSet<ResourceScope>,
    /// Scopes of `scopes` whose rows the session reads: whole-row read and an installed key.
    pub readable: BTreeSet<ResourceScope>,
    /// Scopes of `scopes` with an insert, update or delete grant.
    pub writable: BTreeSet<ResourceScope>,
}

/// How an update publishes the values it assigns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UpdateMode {
    /// Only values that differ from the row before the transaction.
    Changed,
    /// Every assigned value, equal or not. Needs rights to each one.
    Assigned,
}

/// A principal's handle on an `EncryptedReplica`. Every call checks that
/// the replica's authority has not changed since the session opened, that
/// the credentials are still valid now and that the scope is in the
/// session's set. A revocation, credential, key or logout change ends it.
pub struct SecureSession<A> {
    pub(super) replica: EncryptedReplica<A>,
    pub(super) authority: u64,
    pub(super) status: SessionStatus,
}
impl<A> Clone for SecureSession<A> {
    fn clone(&self) -> Self {
        Self {
            replica: self.replica.clone(),
            authority: self.authority,
            status: self.status.clone(),
        }
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// Opens a session over `scopes`, each of which the node's credentials
    /// must name.
    pub async fn session(
        &self,
        scopes: BTreeSet<ResourceScope>,
    ) -> Result<SecureSession<A>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let status = inner.session_status(&scopes, now()?)?;
        Ok(SecureSession {
            replica: self.clone(),
            authority: inner.authority,
            status,
        })
    }
    /// Commits `writes` as one bundle, which may span scopes and exceed one
    /// frame. The frames are returned after a flush; the same handle and
    /// writes return them again, other writes conflict.
    pub async fn author_bundle(
        &self,
        handle: Commitment,
        writes: Vec<RowWrite>,
        dependencies: Vec<Hash>,
    ) -> Result<Vec<EncryptedFrame>, Error> {
        self.bundle_with(handle, writes, dependencies, None).await
    }
    async fn bundle_with(
        &self,
        handle: Commitment,
        writes: Vec<RowWrite>,
        dependencies: Vec<Hash>,
        authority: Option<u64>,
    ) -> Result<Vec<EncryptedFrame>, Error> {
        let encoded = encode(&(&writes, &dependencies))?;
        if encoded.len() > MAX_BUNDLE_BYTES {
            return Err(Error::Quota {
                resource: "bundle bytes",
                limit: MAX_BUNDLE_BYTES,
            });
        }
        let work = group::Work::Bundle {
            handle,
            input: rocky_core::commitment("rocky.secure-bundle-input.v1", &encoded),
            writes,
            dependencies,
            authority,
        };
        match self
            .submit(vec![(work, encoded.len())], true)
            .await
            .pop()
            .unwrap_or_else(|| Err(denied("missing authoring result")))?
        {
            (group::Done::Bundle(frames), _) => Ok(frames),
            (group::Done::Pruned(_), _) => Err(denied(
                "mutation handle was committed before the pruned prefix",
            )),
            _ => Err(denied("missing authoring result")),
        }
    }
    /// Replaces the node's credential chain, as a renewal or attenuation.
    /// Keys the new grants cannot read whole are evicted, and every session
    /// ends.
    pub async fn renew_credentials(&self, credentials: CredentialChain) -> Result<(), Error> {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        let grants = inner.resolve_chain(
            &credentials,
            inner.config.node.public_key(),
            &inner.config.revocations,
            now()?,
        )?;
        if credentials.author_number().map_err(credential)?
            != inner
                .config
                .credentials
                .author_number()
                .map_err(credential)?
        {
            return Err(denied("a node cannot change its author number"));
        }
        inner.chain = Chain::new(credentials.clone())?;
        inner.config.credentials = credentials;
        // The renewed chain is the original node's; a line node of another
        // line keeps authoring there.
        let original = identity::NodeAuthor::new(&inner.config)?;
        let root = inner
            .config
            .manifest
            .verify(inner.config.publisher)
            .map_err(credential)?
            .hash();
        inner
            .line_nodes
            .insert(rocky_credentials::genesis_tip(root), original.clone());
        if inner
            .node_author
            .as_ref()
            .is_none_or(|author| author.key.public_key() == inner.config.node.public_key())
        {
            let on_genesis = match &inner.workspace.followed {
                Some((domain, _)) => inner
                    .graph_for(*domain)
                    .and_then(|graph| {
                        graph
                            .line_root(inner.workspace.tip)
                            .map_err(credential)
                            .map(|line| line == graph.genesis())
                    })
                    .unwrap_or(true),
                None => true,
            };
            if on_genesis {
                inner.node_author = Some(original);
            }
        }
        let readable =
            |key: &KeyMaterial| grants.iter().any(|grant| readable_grant(grant, key.scope));
        inner.config.keys.retain(readable);
        inner.history.retain(readable);
        inner.bump_authority();
        Ok(())
    }
    /// Ends every session and drops all keys from memory, so nothing more
    /// is decrypted or authored. Authorized plaintext already stored stays
    /// at rest; deleting it is the deployment's decision.
    pub async fn logout(&self) {
        let mut inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.config.keys.clear();
        inner.history.clear();
        inner.bump_authority();
    }
}

fn readable_grant(grant: &rocky_core::ResourceGrant, scope: ResourceScope) -> bool {
    grant.scope == scope && grant.rows == RowSelection::All && grant.rights.contains(&Right::Read)
}

impl<A: RowApplication> Inner<A> {
    fn session_status(
        &self,
        scopes: &BTreeSet<ResourceScope>,
        time: u64,
    ) -> Result<SessionStatus, Error> {
        if scopes.is_empty() || scopes.len() > 256 {
            return Err(denied("session scope quota"));
        }
        let grants = self.local_grants(time)?;
        let mut status = SessionStatus {
            principal: self
                .config
                .credentials
                .principal()
                .ok_or_else(|| denied("missing user credential"))?,
            node: self.config.node.public_key(),
            manifest: self.manifest,
            revocations: self.revocation_digest()?,
            revoked: self.config.revocations.live(time),
            expires: self
                .config
                .credentials
                .0
                .iter()
                .map(|signed| signed.capability.expires)
                .min()
                .unwrap_or(0),
            scopes: scopes.clone(),
            readable: BTreeSet::new(),
            writable: BTreeSet::new(),
        };
        for scope in scopes {
            let named: Vec<_> = grants
                .iter()
                .filter(|grant| grant.scope == *scope)
                .collect();
            if named.is_empty() {
                return Err(denied("session scope is not granted"));
            }
            if named.iter().any(|grant| readable_grant(grant, *scope))
                && self.config.keys.iter().any(|key| key.scope == *scope)
            {
                status.readable.insert(*scope);
            }
            if named.iter().any(|grant| {
                [Right::Insert, Right::Update, Right::Delete]
                    .iter()
                    .any(|right| grant.rights.contains(right))
            }) {
                status.writable.insert(*scope);
            }
        }
        Ok(status)
    }
    /// Refuses a session opened under an earlier authority.
    pub(super) fn check_authority(&self, authority: Option<u64>) -> Result<(), Error> {
        match authority {
            Some(authority) if authority != self.authority => {
                Err(Error::Sql(crate::sql::SqlError::Unavailable {
                    table: "session".into(),
                    reason: crate::sql::Unavailable::Revoked,
                }))
            }
            _ => Ok(()),
        }
    }
    /// Checks a session call: authority, current credentials and scope.
    fn admit_session(
        &self,
        session: &SessionStatus,
        authority: u64,
        scope: ResourceScope,
    ) -> Result<Grants, Error> {
        self.check_authority(Some(authority))?;
        if !session.scopes.contains(&scope) {
            return Err(denied("scope is outside the session"));
        }
        self.local_grants(now()?)
    }
    /// A row as a writer sees it: None when absent or deleted. Requires the
    /// scope's key and whole-row read authority.
    async fn image(
        &self,
        scope: ResourceScope,
        row: Commitment,
    ) -> Result<Option<RowFields>, Error> {
        if !self.readable(scope)? {
            return Err(denied("scope key unavailable"));
        }
        Ok(self
            .key_row(scope, row)
            .await?
            .filter(|record| !record.deleted)
            .map(|record| record.fields))
    }
}

impl<A: RowApplication> SecureSession<A> {
    /// The session's bindings, after checking it is still valid.
    pub async fn status(&self) -> Result<SessionStatus, Error> {
        self.status_with(crate::replica::Class::Background).await
    }
    pub(super) async fn status_with(
        &self,
        class: crate::replica::Class,
    ) -> Result<SessionStatus, Error> {
        let inner = self.replica.inner.lock(class).await;
        inner.check_authority(Some(self.authority))?;
        inner.local_grants(now()?)?;
        Ok(self.status.clone())
    }
    pub async fn read(&self, scope: ResourceScope, row: Commitment) -> Result<Companion, Error> {
        let inner = self
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        inner.admit_session(&self.status, self.authority, scope)?;
        inner.companion(scope, row).await
    }
    pub async fn page(
        &self,
        scope: ResourceScope,
        after: Option<Commitment>,
        max_rows: usize,
    ) -> Result<SecurePage, Error> {
        if max_rows == 0 || max_rows > MAX_SNAPSHOT_ROWS {
            return Err(Error::Quota {
                resource: "encrypted snapshot",
                limit: MAX_SNAPSHOT_ROWS,
            });
        }
        let inner = self
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        inner.admit_session(&self.status, self.authority, scope)?;
        inner.page(scope, after, max_rows, MAX_SNAPSHOT_BYTES).await
    }
    pub fn transaction(&self) -> SecureTransaction<A> {
        SecureTransaction {
            session: self.clone(),
            rows: BTreeMap::new(),
        }
    }
}

/// One staged row: its image when the transaction first touched it, its
/// current image and the fields updates assigned.
#[derive(Clone)]
struct Staged {
    before: Option<RowFields>,
    after: Option<RowFields>,
    assigned: BTreeSet<Commitment>,
}

/// Writes staged in memory and committed as one bundle. Each call is
/// admitted against the session's current authority; the commit admits the
/// exact write plan again under the replica lock. Dropping it discards the
/// writes. A row another writer changed meanwhile fails the commit with
/// `Error::Conflict`.
pub struct SecureTransaction<A> {
    session: SecureSession<A>,
    rows: BTreeMap<(ResourceScope, Commitment), Staged>,
}
impl<A: RowApplication> SecureTransaction<A> {
    /// The row with this transaction's writes.
    pub async fn get(&mut self, scope: ResourceScope, row: Commitment) -> Result<Companion, Error> {
        self.edit(scope, row, |_| Ok(()))
            .await
            .map(|after| match after {
                Some(fields) => Companion::Present(fields),
                None => Companion::Deleted,
            })
    }
    pub async fn insert(
        &mut self,
        scope: ResourceScope,
        row: Commitment,
        fields: RowFields,
    ) -> Result<(), Error> {
        self.edit(scope, row, |staged| {
            if staged.after.is_some() {
                return Err(denied("row already exists"));
            }
            staged.after = Some(fields);
            Ok(())
        })
        .await
        .map(drop)
    }
    /// Sets `values` on an existing row; other fields keep their values.
    pub async fn update(
        &mut self,
        scope: ResourceScope,
        row: Commitment,
        values: RowFields,
        mode: UpdateMode,
    ) -> Result<(), Error> {
        self.edit(scope, row, |staged| {
            let after = staged
                .after
                .as_mut()
                .ok_or_else(|| denied("row does not exist"))?;
            if mode == UpdateMode::Assigned {
                staged.assigned.extend(values.keys().copied());
            }
            after.extend(values);
            Ok(())
        })
        .await
        .map(drop)
    }
    pub async fn delete(&mut self, scope: ResourceScope, row: Commitment) -> Result<(), Error> {
        self.edit(scope, row, |staged| {
            if staged.after.take().is_none() {
                return Err(denied("row does not exist"));
            }
            staged.assigned.clear();
            Ok(())
        })
        .await
        .map(drop)
    }
    /// The checked write plan: one write per changed row. Unchanged
    /// permission-controlling fields are not republished, since mergeable
    /// writes to them are refused.
    pub async fn writes(&self) -> Result<Vec<RowWrite>, Error> {
        let inner = self
            .session
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        Ok(self
            .rows
            .iter()
            .filter_map(|((scope, row), staged)| plan(&inner, *scope, *row, staged))
            .collect())
    }
    /// Commits every write as one bundle under `handle` and returns its
    /// frames after a flush. Nothing is committed when nothing changed.
    pub async fn commit(self, handle: Commitment) -> Result<Vec<EncryptedFrame>, Error> {
        let writes = self.writes().await?;
        if writes.is_empty() {
            return Ok(Vec::new());
        }
        self.session
            .replica
            .bundle_with(handle, writes, Vec::new(), Some(self.session.authority))
            .await
    }
    /// Admits a call, loads the row on first use and applies `change` to
    /// it. A change the current grants do not permit is refused at once and
    /// leaves the staged row as it was; the commit checks again. Returns the
    /// row's current image.
    async fn edit(
        &mut self,
        scope: ResourceScope,
        row: Commitment,
        change: impl FnOnce(&mut Staged) -> Result<(), Error>,
    ) -> Result<Option<RowFields>, Error> {
        let inner = self
            .session
            .replica
            .inner
            .lock(crate::replica::Class::Background)
            .await;
        let grants = inner.admit_session(&self.session.status, self.session.authority, scope)?;
        if !self.rows.contains_key(&(scope, row)) {
            if self.rows.len() >= MAX_BUNDLE_ROWS {
                return Err(Error::Quota {
                    resource: "bundle rows",
                    limit: MAX_BUNDLE_ROWS,
                });
            }
            let image = inner.image(scope, row).await?;
            self.rows.insert(
                (scope, row),
                Staged {
                    before: image.clone(),
                    after: image,
                    assigned: BTreeSet::new(),
                },
            );
        }
        let staged = self
            .rows
            .get_mut(&(scope, row))
            .ok_or_else(|| Error::Corrupt("staged row".into()))?;
        let mut next = staged.clone();
        change(&mut next)?;
        if plan(&inner, scope, row, &next).is_some_and(|write| {
            !write.permitted_by(&grants)
                || write
                    .published_fields()
                    .iter()
                    .any(|field| inner.permission_fields.contains(field))
        }) {
            return Err(denied(
                "row/field transition denied; permission fields never change by row writes",
            ));
        }
        *staged = next;
        Ok(staged.after.clone())
    }
}

/// The write a staged row publishes, if any.
fn plan<A>(
    inner: &Inner<A>,
    scope: ResourceScope,
    row: Commitment,
    staged: &Staged,
) -> Option<RowWrite> {
    let change = RowChange {
        scope,
        row,
        before: staged.before.clone(),
        after: staged.after.clone(),
    };
    let assigned = match (&change.before, &change.after) {
        (Some(before), Some(after)) => staged
            .assigned
            .iter()
            .filter(|field| after.contains_key(*field))
            .filter(|field| {
                !inner.permission_fields.contains(*field) || before.get(*field) != after.get(*field)
            })
            .copied()
            .collect(),
        _ => BTreeSet::new(),
    };
    let write = RowWrite { change, assigned };
    write.right().map(|_| write)
}
