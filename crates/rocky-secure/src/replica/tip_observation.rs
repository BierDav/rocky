//! What this replica observes of its tips: the frontier a seal of the
//! followed tip would name, progress since the last seal and peers' last
//! reported frontiers. Peer holdings are unknown unless a peer reports them.
use super::*;
use rocky_core::Scope;
use rocky_credentials::{AuthorMark, PeerId, TipGraph};

/// One resource scope's per-author frontier, as a peer last reported it.
#[derive(Clone, Debug)]
pub struct PeerScopeFrontier {
    pub scope: ResourceScope,
    pub logs: Vec<FrontierLog>,
}
/// A peer of the domain: the tip its open link claims and its frontiers.
#[derive(Clone, Debug)]
pub struct TipDevice {
    pub node: PeerId,
    /// Unknown without an open link.
    pub tip: Option<TipId>,
    pub frontiers: Vec<PeerScopeFrontier>,
}
#[derive(Clone, Debug)]
pub struct TipObservation {
    pub bookmark: Option<String>,
    pub tip: TipId,
    /// Locally held operations visible in the followed tip beyond the seal
    /// it continues from, or beyond its own seal once sealed.
    pub operations_since_seal: u64,
    pub devices: Vec<TipDevice>,
}
/// Which tips and seals a node follows or keeps; `None` is unknown.
#[derive(Clone, Debug)]
pub struct TipNodeObservation {
    pub node: PeerId,
    pub followed_tip: Option<TipId>,
    pub kept_tips: Option<Vec<TipId>>,
    pub kept_seals: Option<Vec<Commitment>>,
}

/// Where a signer's writes of an earlier key epoch stand for members.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EarlierEpochWrites {
    /// A seal and a served base at it cover each.
    Covered,
    /// One lies past the followed tip's seal: a seal must follow.
    Unsealed,
    /// One is sealed, but past the base its scope serves: a base at a
    /// later seal must replace it.
    Unbased,
}

impl<A: RowApplication> Inner<A> {
    /// Every locally held operation and void header of the domain. A scan
    /// of headers, never of rows; explicit developer calls only.
    pub(super) async fn domain_headers(&self, scope: Scope) -> Result<Vec<Extensions>, Error> {
        let mut headers = Vec::new();
        for row in rows(
            &self.conn,
            "SELECT header FROM secure_operations UNION ALL SELECT header FROM secure_voids",
        )
        .await?
        {
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(Error::Corrupt("tip observation header".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            if header.extensions.scope.domain == scope {
                headers.push(header.extensions);
            }
        }
        Ok(headers)
    }
    fn visible_header(&self, graph: &TipGraph, header: &Extensions) -> Result<bool, Error> {
        let author = header.author.ok_or_else(|| denied("node author missing"))?;
        self.visible_frame(
            graph,
            header.tip,
            author,
            header.author_sequence,
            self.workspace.tip,
        )
    }
    /// The seal frontier from one unpaged header scan, for the transitions
    /// that act on it in the same acquisition: partition creation and tip
    /// statement admission.
    pub(crate) async fn seal_frontier(
        &self,
        scope: Scope,
        close: bool,
    ) -> Result<Vec<AuthorMark>, Error> {
        if close {
            self.refuse_pruned_close(scope).await?;
        }
        let graph = self.scope_graph(scope)?;
        let mut marks = self.inherited_marks(&graph)?;
        self.fold_marks(&graph, &mut marks, self.domain_headers(scope).await?)?;
        Ok(into_marks(marks))
    }
    /// A close seal needs retained history: pruned logs have no author frontier.
    async fn refuse_pruned_close(&self, scope: Scope) -> Result<(), Error> {
        for record in rows(&self.conn, "SELECT id,scope FROM secure_scope_state").await? {
            let [SqlValue::Integer(id), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope".into()));
            };
            let resource: ResourceScope = decode(bytes)?;
            if resource.domain == scope && self.has_pruned_prefix(resource).await? {
                // A base on the inherited path is already bounded by its seal's
                // author frontier. Only uncovered pruning prevents a close.
                if let Some(base) = self.base_seal(*id).await?
                    && self
                        .path_seals(scope)?
                        .iter()
                        .any(|seal| seal.id().ok() == Some(base))
                {
                    continue;
                }
                return Err(denied(
                    "a close seal needs retained history; pruned logs have no author frontier",
                ));
            }
        }
        Ok(())
    }
    fn inherited_marks(&self, graph: &TipGraph) -> Result<BTreeMap<u32, u64>, Error> {
        Ok(Self::tip_parent(graph, self.workspace.tip)?
            .map(|(_, inherited)| {
                inherited
                    .iter()
                    .map(|mark| (mark.author, mark.sequence))
                    .collect()
            })
            .unwrap_or_default())
    }
    fn fold_marks(
        &self,
        graph: &TipGraph,
        marks: &mut BTreeMap<u32, u64>,
        headers: Vec<Extensions>,
    ) -> Result<(), Error> {
        for header in headers {
            if self.visible_header(graph, &header)? {
                let author = header.author.ok_or_else(|| denied("node author missing"))?;
                let mark = marks.entry(author).or_default();
                *mark = (*mark).max(header.author_sequence);
            }
        }
        Ok(())
    }
    /// The frontier a seal of the followed tip names now.
    fn sealed_baseline(&self, graph: &TipGraph) -> Result<Vec<AuthorMark>, Error> {
        Ok(match graph.seal(self.workspace.tip).map_err(credential)? {
            Some(seal) => seal.statement.frontier.clone(),
            None => Self::tip_parent(graph, self.workspace.tip)?
                .map(|(_, frontier)| frontier)
                .unwrap_or_default(),
        })
    }
    async fn observed_devices(&self, scope: Scope) -> Result<Vec<TipDevice>, Error> {
        let mut peers = BTreeMap::<PeerId, BTreeMap<ResourceScope, Vec<FrontierLog>>>::new();
        for row in rows(
            &self.conn,
            "SELECT peer,scope,author,held,applied FROM secure_peer_frontiers ORDER BY peer,scope,author",
        )
        .await?
        {
            let [
                SqlValue::Blob(peer),
                SqlValue::Blob(resource),
                SqlValue::Blob(author),
                SqlValue::Integer(held),
                SqlValue::Integer(applied),
            ] = row.as_slice()
            else {
                return Err(Error::Corrupt("peer frontier".into()));
            };
            let resource: ResourceScope = decode(resource)?;
            if resource.domain != scope {
                continue;
            }
            let peer = PeerId::from_bytes(
                peer.as_slice()
                    .try_into()
                    .map_err(|_| Error::Corrupt("peer id".into()))?,
            );
            let count = |value: &i64| {
                u64::try_from(*value).map_err(|_| Error::Corrupt("negative peer frontier".into()))
            };
            peers
                .entry(peer)
                .or_default()
                .entry(resource)
                .or_default()
                .push(FrontierLog {
                    author: checkpoint::key_of(author)?,
                    held: count(held)?,
                    applied: count(applied)?,
                });
        }
        for peer in self.peer_tips.keys() {
            peers.entry(*peer).or_default();
        }
        Ok(peers
            .into_iter()
            .map(|(node, frontiers)| TipDevice {
                node,
                tip: self.peer_tips.get(&node).copied(),
                frontiers: frontiers
                    .into_iter()
                    .map(|(scope, logs)| PeerScopeFrontier { scope, logs })
                    .collect(),
            })
            .collect())
    }
}

fn into_marks(marks: BTreeMap<u32, u64>) -> Vec<AuthorMark> {
    marks
        .into_iter()
        .map(|(author, sequence)| AuthorMark { author, sequence })
        .collect()
}
fn covers(frontier: &[AuthorMark], header: &Extensions, author: u32) -> bool {
    frontier
        .iter()
        .any(|mark| mark.author == author && header.author_sequence <= mark.sequence)
}

impl<A: RowApplication> EncryptedReplica<A> {
    /// The global author marks of every locally held write visible in the
    /// followed tip, at least its parent seal's. Pruned writes have no
    /// known position and are left out: like writes the sealer never held,
    /// they walk a `continue` seal as late frames. A `close` would hold
    /// them where others applied them, so it refuses pruned history.
    pub async fn seal_frontier(&self, scope: Scope, close: bool) -> Result<Vec<AuthorMark>, Error> {
        self.fold_headers(
            scope,
            |inner| {
                let graph = inner.scope_graph(scope)?;
                Ok((inner.inherited_marks(&graph)?, graph))
            },
            async |inner, (marks, graph), headers| inner.fold_marks(graph, marks, headers),
            async |inner, (marks, _)| {
                if close {
                    inner.refuse_pruned_close(scope).await?;
                }
                Ok(into_marks(marks))
            },
        )
        .await
    }
    /// Where writes visible in the followed tip, under an earlier key epoch
    /// than their scope's current one, stand: members given only the
    /// current epoch get their rows with a checkpoint at a later seal, never
    /// by opening the frame. Scans only after such a frame applied or the
    /// followed tip moved, and again until they are covered.
    pub async fn earlier_epoch_writes(&self, scope: Scope) -> Result<EarlierEpochWrites, Error> {
        if !self.earlier_epoch_due().await {
            return Ok(EarlierEpochWrites::Covered);
        }
        let found = self
            .fold_headers(
                scope,
                |inner| {
                    let graph = inner.scope_graph(scope)?;
                    let sealed = inner.sealed_baseline(&graph)?;
                    Ok(EpochScan {
                        graph,
                        sealed,
                        bases: BTreeMap::new(),
                        found: EarlierEpochWrites::Covered,
                    })
                },
                async |inner, scan, headers| {
                    if scan.found != EarlierEpochWrites::Unsealed {
                        inner.scan_earlier_epochs(scan, headers).await?;
                    }
                    Ok(())
                },
                async |inner, scan| {
                    inner
                        .earlier_epoch
                        .store(scan.found != EarlierEpochWrites::Covered, Ordering::Relaxed);
                    inner.epochs_scanned = Some(inner.workspace.tip);
                    Ok(scan.found)
                },
            )
            .await?;
        Ok(found)
    }
    /// Whether `unsealed_earlier_epochs` would scan.
    pub async fn earlier_epoch_due(&self) -> bool {
        self.inner
            .lock(crate::replica::Class::Background)
            .await
            .earlier_epoch_due()
    }
}

impl<A: RowApplication> Inner<A> {
    fn earlier_epoch_due(&self) -> bool {
        self.earlier_epoch.load(Ordering::Relaxed)
            || self.epochs_scanned != Some(self.workspace.tip)
    }
}

struct EpochScan {
    graph: TipGraph,
    sealed: Vec<AuthorMark>,
    /// The frontier of the seal each served base sits at.
    bases: BTreeMap<ResourceScope, Option<Vec<AuthorMark>>>,
    found: EarlierEpochWrites,
}

impl<A: RowApplication> Inner<A> {
    async fn scan_earlier_epochs(
        &mut self,
        scan: &mut EpochScan,
        headers: Vec<Extensions>,
    ) -> Result<(), Error> {
        for header in headers {
            let Some(author) = header.author else {
                continue;
            };
            let current = self
                .config
                .keys
                .iter()
                .find(|key| key.scope == header.scope)
                .map(|key| key.generation);
            if current.is_none_or(|current| header.generation >= current)
                || !self.visible_header(&scan.graph, &header)?
            {
                continue;
            }
            if !covers(&scan.sealed, &header, author) {
                scan.found = EarlierEpochWrites::Unsealed;
                return Ok(());
            }
            if !scan.bases.contains_key(&header.scope) {
                let frontier = match self.served_checkpoint(header.scope).await? {
                    Some((_, signed)) => match signed.statement.seal {
                        Some(seal) => Some(
                            scan.graph
                                .seal_by_id(seal)
                                .map_err(credential)?
                                .statement
                                .frontier
                                .clone(),
                        ),
                        None => None,
                    },
                    None => None,
                };
                scan.bases.insert(header.scope, frontier);
            }
            if let Some(Some(base)) = scan.bases.get(&header.scope)
                && !covers(base, &header, author)
            {
                scan.found = EarlierEpochWrites::Unbased;
            }
        }
        Ok(())
    }
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn tip_observation(&self, scope: Scope) -> Result<TipObservation, Error> {
        self.fold_headers(
            scope,
            |inner| {
                let graph = inner.scope_graph(scope)?;
                Ok((inner.sealed_baseline(&graph)?, graph, 0))
            },
            async |inner, (baseline, graph, operations), headers| {
                for header in headers {
                    let author = header.author.ok_or_else(|| denied("node author missing"))?;
                    if !covers(baseline, &header, author) && inner.visible_header(graph, &header)? {
                        *operations += 1;
                    }
                }
                Ok(())
            },
            async |inner, (_, _, operations_since_seal)| {
                Ok(TipObservation {
                    bookmark: inner
                        .workspace
                        .followed
                        .as_ref()
                        .filter(|(domain, _)| *domain == scope)
                        .map(|(_, name)| name.clone()),
                    tip: inner.workspace.tip,
                    operations_since_seal,
                    devices: inner.observed_devices(scope).await?,
                })
            },
        )
        .await
    }
    /// This node's holdings, with the seals it keeps, and the tips peers'
    /// open links claim; peers report no holdings.
    pub async fn tip_nodes(&self, scope: Scope) -> Result<Vec<TipNodeObservation>, Error> {
        self.fold_headers(
            scope,
            |inner| {
                inner.scope_graph(scope)?;
                Ok(BTreeSet::from([inner.workspace.tip]))
            },
            async |_, tips, headers| {
                tips.extend(headers.into_iter().map(|header| header.tip));
                Ok(())
            },
            async |inner, tips| {
                let local = PeerId::of(inner.config.node.public_key());
                let mut nodes = vec![TipNodeObservation {
                    node: local,
                    followed_tip: Some(inner.workspace.tip),
                    kept_tips: Some(tips.into_iter().collect()),
                    kept_seals: Some(
                        inner
                            .kept_marks()
                            .await?
                            .into_iter()
                            .map(|(seal, _)| seal)
                            .collect(),
                    ),
                }];
                for device in inner.observed_devices(scope).await? {
                    if device.node != local {
                        nodes.push(TipNodeObservation {
                            node: device.node,
                            followed_tip: device.tip,
                            kept_tips: None,
                            kept_seals: None,
                        });
                    }
                }
                Ok(nodes)
            },
        )
        .await
    }
    /// Whether the followed bookmark's line lacks a registered node, so
    /// writes wait for `register_line_node`.
    pub async fn line_node_needed(&self) -> bool {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.workspace.followed.is_some() && inner.node_author.is_none()
    }
}
