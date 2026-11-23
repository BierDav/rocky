use super::*;
use rocky_core::Scope;
use rocky_credentials::{AuthorMark, PeerId, TipGraph};

#[derive(Clone, Debug)]
pub struct PeerScopeFrontier {
    pub scope: ResourceScope,
    pub logs: Vec<FrontierLog>,
}
#[derive(Clone, Debug)]
pub struct TipDevice {
    pub node: PeerId,
    pub tip: Option<TipId>,
    pub frontiers: Vec<PeerScopeFrontier>,
}
#[derive(Clone, Debug)]
pub struct TipObservation {
    pub bookmark: Option<String>,
    pub tip: TipId,
    pub operations_since_seal: u64,
    pub devices: Vec<TipDevice>,
}
#[derive(Clone, Debug)]
pub struct TipNodeObservation {
    pub node: PeerId,
    pub followed_tip: Option<TipId>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EarlierEpochWrites {
    Covered,
    Unsealed,
    Unbased,
}

impl<A: RowApplication> Inner<A> {
    pub(super) async fn domain_headers(
        &self,
        scope: Scope,
    ) -> Result<Vec<headers::ScannedHeader>, Error> {
        self.domain_headers_from(scope, true).await
    }
    async fn domain_headers_from(
        &self,
        scope: Scope,
        pending: bool,
    ) -> Result<Vec<headers::ScannedHeader>, Error> {
        let mut headers = Vec::new();
        for row in rows(
            &self.conn,
            if pending {
                "SELECT header FROM secure_operations UNION ALL SELECT header FROM secure_voids"
            } else {
                "SELECT header FROM secure_operations WHERE status<>'pending' UNION ALL SELECT header FROM secure_voids"
            },
        )
        .await?
        {
            #[cfg(any(test, feature = "qualification"))]
            self.flusher
                .measurements
                .work(lock::WorkUnit::DomainHeader, 1);
            let [SqlValue::Blob(bytes)] = row.as_slice() else {
                return Err(Error::Corrupt("tip observation header".into()));
            };
            let header: p2panda_core::Header<Extensions> = decode(bytes)?;
            if header.extensions.scope.domain == scope {
                headers.push(headers::ScannedHeader::new(header));
            }
        }
        Ok(headers)
    }
    fn visible_header(
        &self,
        graph: &TipGraph,
        header: &headers::ScannedHeader,
    ) -> Result<bool, Error> {
        let author = header.author.ok_or_else(|| denied("node author missing"))?;
        self.visible_frame(
            graph,
            header.tip,
            author,
            header.author_sequence,
            self.workspace.tip,
        )
    }
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
        let headers = self
            .domain_headers_from(scope, self.engine_matches())
            .await?;
        self.fold_marks(&graph, &mut marks, headers)?;
        Ok(into_marks(marks))
    }
    async fn refuse_pruned_close(&self, scope: Scope) -> Result<(), Error> {
        for record in rows(&self.conn, "SELECT id,scope FROM secure_scope_state").await? {
            let [SqlValue::Integer(id), SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope".into()));
            };
            let resource: ResourceScope = decode(bytes)?;
            if resource.domain == scope && self.has_pruned_prefix(resource).await? {
                let pruning_bounded_by_inherited_base = match self.base_seal(*id).await? {
                    Some(base) => self
                        .path_seals(scope)?
                        .iter()
                        .any(|seal| seal.id().ok() == Some(base)),
                    None => false,
                };
                if pruning_bounded_by_inherited_base {
                    continue;
                }
                return Err(denied(
                    "a close seal needs retained history; pruned logs have no author frontier",
                ));
            }
        }
        Ok(())
    }
    fn inherited_marks(&self, graph: &TipGraph) -> Result<BTreeMap<WriterId, u64>, Error> {
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
        marks: &mut BTreeMap<WriterId, u64>,
        headers: Vec<headers::ScannedHeader>,
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
        for peer in self.known_peers.admitted() {
            peers.entry(*peer).or_default();
        }
        Ok(peers
            .into_iter()
            .map(|(node, frontiers)| TipDevice {
                node,
                tip: self.known_peers.tip(&node),
                frontiers: frontiers
                    .into_iter()
                    .map(|(scope, logs)| PeerScopeFrontier { scope, logs })
                    .collect(),
            })
            .collect())
    }
}

fn into_marks(marks: BTreeMap<WriterId, u64>) -> Vec<AuthorMark> {
    marks
        .into_iter()
        .map(|(author, sequence)| AuthorMark { author, sequence })
        .collect()
}
fn covers(frontier: &[AuthorMark], header: &Extensions, author: WriterId) -> bool {
    frontier
        .iter()
        .any(|mark| mark.author == author && header.author_sequence <= mark.sequence)
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn seal_frontier(&self, scope: Scope, close: bool) -> Result<Vec<AuthorMark>, Error> {
        {
            let inner = self.inner.lock(crate::replica::Class::Background).await;
            if !inner.engine_matches() {
                return inner.seal_frontier(scope, close).await;
            }
        }
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
    graph: Arc<TipGraph>,
    sealed: Vec<AuthorMark>,
    bases: BTreeMap<ResourceScope, Option<Vec<AuthorMark>>>,
    found: EarlierEpochWrites,
}

impl<A: RowApplication> Inner<A> {
    async fn scan_earlier_epochs(
        &mut self,
        scan: &mut EpochScan,
        headers: Vec<headers::ScannedHeader>,
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
    pub async fn tip_nodes(&self, scope: Scope) -> Result<Vec<TipNodeObservation>, Error> {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.scope_graph(scope)?;
        let local = PeerId::of(inner.config.node.public_key());
        let mut nodes = vec![TipNodeObservation {
            node: local,
            followed_tip: Some(inner.workspace.tip),
        }];
        for device in inner.observed_devices(scope).await? {
            if device.node != local {
                nodes.push(TipNodeObservation {
                    node: device.node,
                    followed_tip: device.tip,
                });
            }
        }
        Ok(nodes)
    }
    pub async fn line_node_needed(&self) -> bool {
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        inner.workspace.followed.is_some() && inner.node_author.is_none()
    }
}
