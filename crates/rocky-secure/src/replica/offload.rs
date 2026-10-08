//! Authorized reads served by a replica that holds a scope, for a peer that
//! does not. The result is never stored or observed as local state.
use super::roles::FrontierLog;
use super::*;

/// Rows read from another replica. It describes that replica's state at its
/// revision and frontier, not this replica's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RemotePage {
    pub source: PublicKey,
    pub scope: ResourceScope,
    /// The source's local revision when it read the page.
    pub revision: u64,
    /// The source held the scope, had its key and no pending operation.
    pub complete: bool,
    /// The source's applied frontier of the scope.
    pub frontier: Vec<FrontierLog>,
    pub rows: BTreeMap<Commitment, RowFields>,
    pub next: Option<Commitment>,
}
#[derive(Serialize, Deserialize)]
struct RemotePlain {
    scope: ResourceScope,
    after: Option<Commitment>,
    revision: u64,
    complete: bool,
    frontier: Vec<FrontierLog>,
    rows: Vec<(Commitment, RowFields)>,
    next: Option<Commitment>,
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// One page of a held scope for a peer that may read all of it, sealed
    /// with the scope key.
    pub async fn remote_read(
        &self,
        peer: PublicKey,
        credentials: &CredentialChain,
        scope: ResourceScope,
        after: Option<Commitment>,
        max_rows: usize,
    ) -> Result<EncryptedPackage, Error> {
        if max_rows == 0 || max_rows > MAX_SNAPSHOT_ROWS {
            return Err(Error::Quota {
                resource: "encrypted snapshot",
                limit: MAX_SNAPSHOT_ROWS,
            });
        }
        let inner = self.inner.lock().await;
        let grants = inner.verify_chain(credentials, peer, now()?)?;
        if !inner.can_read_key(scope, &grants) {
            return Err(denied("remote read needs whole-scope read authority"));
        }
        if !inner.holds(scope).await? {
            return Err(denied("scope is not held by this replica"));
        }
        let (generation, key) = inner.scope_key(scope)?;
        // Half the frame for rows: byte strings may double when encoded.
        let page = inner
            .page(scope, after, max_rows, MAX_SECURE_FRAME / 4)
            .await?;
        let frontier = inner.frontier(scope).await?;
        let plain = RemotePlain {
            scope,
            after,
            revision: page.revision,
            complete: page.complete,
            frontier: frontier.logs,
            rows: page.rows.into_iter().collect(),
            next: page.next,
        };
        EncryptedPackage::seal(scope, generation, PackageKind::Read, &key, &encode(&plain)?)
    }
    /// Opens a remote read with the local scope key. Nothing is stored.
    pub async fn open_remote_read(
        &self,
        source: PublicKey,
        scope: ResourceScope,
        after: Option<Commitment>,
        package: &EncryptedPackage,
    ) -> Result<RemotePage, Error> {
        let inner = self.inner.lock().await;
        if !inner.can_read_key(scope, &inner.local_grants(now()?)?) {
            return Err(denied("remote read needs whole-scope read authority"));
        }
        let key = inner
            .opening_key(scope, package.generation)
            .ok_or_else(|| denied("remote read key unavailable"))?;
        if package.scope != scope || package.kind != PackageKind::Read {
            return Err(denied("remote read outside request"));
        }
        let plain: RemotePlain = decode(&package.open(&key.key)?)?;
        if plain.scope != scope || plain.after != after || plain.rows.len() > MAX_SNAPSHOT_ROWS {
            return Err(denied("remote read outside request"));
        }
        let rows: BTreeMap<_, _> = plain.rows.into_iter().collect();
        if rows
            .keys()
            .any(|row| after.is_some_and(|after| *row <= after))
            || plain
                .next
                .is_some_and(|next| after.is_some_and(|after| next <= after))
        {
            return Err(denied("remote read rows out of order"));
        }
        Ok(RemotePage {
            source,
            scope,
            revision: plain.revision,
            complete: plain.complete,
            frontier: plain.frontier,
            rows,
            next: plain.next,
        })
    }
}
