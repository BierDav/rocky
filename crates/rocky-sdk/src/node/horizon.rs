//! Registry participants, independent of contacts or prior sessions.
use super::*;
use crate::registry::Record;
use rocky_core::{Right, RowSelection};
use rocky_credentials::HorizonPeer;

impl Node {
    pub(super) async fn horizon_participants(
        &self,
        request: &Value,
        scopes: &[ResourceScope],
    ) -> Result<Vec<HorizonPeer>, Failure> {
        let registry = self.registry().await?;
        let settings = self.settings.lock().await;
        let domain = self.domain();
        let holds = |record: &Record, scope: ResourceScope| -> Result<bool, Failure> {
            let Some((name, base)) = domain.scopes().find(|(_, base)| base.table == scope.table)
            else {
                return Ok(false);
            };
            let grants = record
                .profile()
                .map(|profile| domain.effective(profile).raw(&domain.resources()))
                .transpose()?
                .unwrap_or_default();
            let whole = grants.iter().any(|grant| {
                grant.scope == name
                    && (grant.rights.contains(&Right::Read)
                        || grant.rights.contains(&Right::Custody))
            });
            if whole || scope == base {
                return Ok(whole);
            }
            let mut address = request.clone();
            address["table"] = name.into();
            let (_, tuple) = self.partition_address(&address)?;
            Ok(record.grants.iter().any(|grant| {
                grant
                    .grant
                    .resolve(scope.domain, scope.table, &tuple)
                    .is_ok_and(|grant| {
                        grant.is_some_and(|grant| {
                            grant.scope == scope
                                && grant.rows == RowSelection::All
                                && (grant.rights.contains(&Right::Read)
                                    || grant.rights.contains(&Right::Custody))
                        })
                    })
            }))
        };
        let eligible = |node: &PublicKey, record: &Record| -> Result<bool, Failure> {
            if record.device.is_none() || settings.revoked_device(node, &registry) {
                return Ok(false);
            }
            for scope in scopes {
                if !holds(record, *scope)? {
                    return Ok(false);
                }
            }
            Ok(true)
        };
        let nodes = match request.get("participants") {
            Some(nodes) => serde_json::from_value::<Vec<PublicKey>>(nodes.clone())?,
            None => {
                let mut nodes = Vec::new();
                for (node, record) in &registry.records {
                    if eligible(node, record)? {
                        nodes.push(*node);
                    }
                }
                nodes
            }
        };
        if nodes.len() > 64 {
            return Err(refused(
                "horizon_participants_full: at most 64 participants",
            ));
        }
        let mut peers = BTreeMap::new();
        for node in nodes {
            let record = registry
                .get(&node)
                .ok_or_else(|| refused("horizon participant has no registry record"))?;
            if !eligible(&node, record)? {
                return Err(refused(
                    "horizon participant is revoked or does not hold the whole cut",
                ));
            }
            let expires = record
                .capability()
                .ok_or_else(|| refused("horizon participant has no recorded expiry"))?
                .expires;
            if peers
                .insert(
                    node,
                    HorizonPeer {
                        node: rocky_credentials::PeerId::of(node),
                        expires,
                    },
                )
                .is_some()
            {
                return Err(refused("duplicate horizon participant"));
            }
        }
        let mut peers: Vec<_> = peers.into_values().collect();
        peers.sort_by_key(|peer| peer.node);
        Ok(peers)
    }
}
