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

impl<A: RowApplication> Inner<A> {
    /// Every locally held operation and void header of the domain. A scan
    /// of headers, never of rows; explicit developer calls only.
    async fn domain_headers(&self, scope: Scope) -> Result<Vec<Extensions>, Error> {
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
        self.visible_frame(graph, header.tip, author, header.author_sequence, self.tip)
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

impl<A: RowApplication> EncryptedReplica<A> {
    /// The global author marks of every locally held write visible in the
    /// followed tip, at least its parent seal's. Refuses pruned history,
    /// whose global author positions are no longer known.
    pub async fn seal_frontier(&self, scope: Scope) -> Result<Vec<AuthorMark>, Error> {
        let inner = self.inner.lock().await;
        let graph = inner.scope_graph(scope)?;
        for record in rows(&inner.conn, "SELECT scope FROM secure_scope_state").await? {
            let [SqlValue::Blob(bytes)] = record.as_slice() else {
                return Err(Error::Corrupt("scope".into()));
            };
            let resource: ResourceScope = decode(bytes)?;
            if resource.domain == scope && inner.has_horizon(resource).await? {
                return Err(denied(
                    "sealing needs retained history; pruned logs have no author frontier",
                ));
            }
        }
        let mut marks = BTreeMap::<u32, u64>::new();
        if let Some((_, inherited)) = Inner::<A>::tip_parent(&graph, inner.tip)? {
            marks.extend(inherited.iter().map(|mark| (mark.author, mark.sequence)));
        }
        for header in inner.domain_headers(scope).await? {
            if inner.visible_header(&graph, &header)? {
                let author = header.author.ok_or_else(|| denied("node author missing"))?;
                let mark = marks.entry(author).or_default();
                *mark = (*mark).max(header.author_sequence);
            }
        }
        Ok(marks
            .into_iter()
            .map(|(author, sequence)| AuthorMark { author, sequence })
            .collect())
    }
    pub async fn tip_observation(&self, scope: Scope) -> Result<TipObservation, Error> {
        let inner = self.inner.lock().await;
        let graph = inner.scope_graph(scope)?;
        let baseline = match graph.seal(inner.tip).map_err(credential)? {
            Some(seal) => seal.statement.frontier.clone(),
            None => Inner::<A>::tip_parent(&graph, inner.tip)?
                .map(|(_, frontier)| frontier)
                .unwrap_or_default(),
        };
        let mut operations_since_seal = 0;
        for header in inner.domain_headers(scope).await? {
            let author = header.author.ok_or_else(|| denied("node author missing"))?;
            let sealed = baseline
                .iter()
                .any(|mark| mark.author == author && header.author_sequence <= mark.sequence);
            if !sealed && inner.visible_header(&graph, &header)? {
                operations_since_seal += 1;
            }
        }
        Ok(TipObservation {
            bookmark: inner
                .followed
                .as_ref()
                .filter(|(domain, _)| *domain == scope)
                .map(|(_, name)| name.clone()),
            tip: inner.tip,
            operations_since_seal,
            devices: inner.observed_devices(scope).await?,
        })
    }
    /// This node's holdings and the tips peers' open links claim. No node
    /// keeps a seal's state before kept seals exist, so this one keeps none.
    pub async fn tip_nodes(&self, scope: Scope) -> Result<Vec<TipNodeObservation>, Error> {
        let inner = self.inner.lock().await;
        inner.scope_graph(scope)?;
        let local = PeerId::of(inner.config.node.public_key());
        let mut tips = BTreeSet::from([inner.tip]);
        for header in inner.domain_headers(scope).await? {
            tips.insert(header.tip);
        }
        let mut nodes = vec![TipNodeObservation {
            node: local,
            followed_tip: Some(inner.tip),
            kept_tips: Some(tips.into_iter().collect()),
            kept_seals: Some(Vec::new()),
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
    }
    /// Whether the followed bookmark's line lacks a registered node, so
    /// writes wait for `register_line_node`.
    pub async fn line_node_needed(&self) -> bool {
        let inner = self.inner.lock().await;
        inner.followed.is_some() && inner.node_author.is_none()
    }
}
