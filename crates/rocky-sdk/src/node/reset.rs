//! The owner reset: "this is everything, sign out everywhere else".
use super::*;
use rocky_credentials::{PeerId, Reset, Revocation, RevocationSet, RootDecision};
use std::collections::BTreeSet;

impl Node {
    /// Ends every node but this one and those `keep` names, including nodes
    /// this store never saw, and fences their data at what this replica
    /// applied. The fence is captured and the statement merged under one
    /// replica lock; the receipt is returned after the local commit. The
    /// statement that ended this device is recorded, never held. A personal
    /// reset keeps only the signer and verified custody principals: another
    /// full reader could hold unfenced data. The held heads of every ended
    /// lane's tip statements stay, as on a revocation by name; lanes past the
    /// bound keep none. Standing and heads come from the replica's set, under
    /// the lock that installs the reset, and the replica refuses a reset ending
    /// this node.
    pub(super) async fn sign_out_everywhere_else(
        &self,
        root: &Root,
        crate::ResetKeep { also, except }: crate::ResetKeep,
    ) -> Result<crate::ResetReceipt, Failure> {
        let own = PrivateKey::from_bytes(&self.vault.get("node")?).public_key();
        let mut custodians = self.known_custodians().await?;
        let graph = self.replica.tip_graph(self.domain().scope).await?;
        let mut settings = self.settings.lock().await;
        let root = crate::enrollment::root_signer(
            root.signer(),
            settings.publisher,
            "signing out everywhere else",
        )?;
        if let Some(ended) = settings.revocation(&self.path)? {
            return Err(Box::new(crate::ResetRefused::DeviceSignedOut {
                statement: ended.id()?,
            }));
        }
        if let Some(custodian) = also
            .iter()
            .find(|node| settings.revocations.revokes_reference(node))
        {
            return Err(Box::new(crate::ResetRefused::CustodianSignedOut {
                custodian: *custodian,
            }));
        }
        let not_custodian = if settings.kind()?.is_personal() {
            also.iter()
                .find(|node| **node != PeerId::of(own) && !custodians.contains(node))
                .copied()
        } else {
            None
        };
        custodians.retain(|node| !except.contains(node));
        let mut keep = custodians;
        keep.extend(also.iter().copied());
        keep.insert(PeerId::of(own));
        let revocations = &settings.revocations;
        let mut cut = Vec::new();
        for identity in graph.chain_identities().map_err(refused)? {
            let lanes = graph
                .lanes_of(&identity)
                .map_err(refused)?
                .into_iter()
                .filter(|lane| {
                    !keep.contains(&PeerId::of(*lane))
                        && !revocations.ends_node_of(identity, *lane, true)
                })
                .collect();
            cut.extend(graph.proposed_cut(&identity, &lanes).map_err(refused)?.0);
        }
        cut.sort();
        cut.truncate(rocky_credentials::MAX_CUT_LANES);
        let (app, database) = (
            settings.domain()?.manifest().app,
            settings.database_context()?,
        );
        let (issuer, time) = (settings.publisher, now()?);
        let credentials = settings.credentials.clone();
        let signed = self
            .replica
            .install_reset(&keep, |held: &RevocationSet, fence| -> Result<_, Failure> {
                if let Some(ended) = held.revoking_holder(&credentials)? {
                    return Err(Box::new(crate::ResetRefused::DeviceSignedOut {
                        statement: ended.id()?,
                    }));
                }
                if let Some(custodian) = also.iter().find(|node| held.revokes_reference(node)) {
                    return Err(Box::new(crate::ResetRefused::CustodianSignedOut {
                        custodian: *custodian,
                    }));
                }
                if let Some(node) = not_custodian {
                    return Err(Box::new(crate::ResetRefused::NotCustodian { node }));
                }
                Ok(rocky_credentials::Authority::owner(root).revoke_in(
                    Revocation {
                        app,
                        issuer,
                        time,
                        mode: rocky_credentials::RevocationMode::Lost,
                        nodes: BTreeSet::new(),
                        identities: BTreeSet::new(),
                        capabilities: BTreeSet::new(),
                        cut,
                        recovery: Some(RootDecision::Reset(Box::new(Reset {
                            follows: held.reset_heads(),
                            keep: keep.clone(),
                            fence,
                        }))),
                    },
                    database,
                )?)
            })
            .await
            .map_err(|error| {
                if error.is::<rocky_secure::ResetEndsSigner>() {
                    Box::new(crate::ResetRefused::EndsSigner)
                } else {
                    error
                }
            })?;
        let follows = signed
            .statement
            .reset()
            .map(|reset| reset.follows.clone())
            .unwrap_or_default();
        let mut draft = settings.clone();
        draft.learn_revocations(&RevocationSet::try_from(vec![signed.clone()])?)?;
        if let Some(device) = &mut draft.personal {
            device.unwitnessed = true;
        }
        draft.save(&self.path)?;
        *settings = draft;
        self.apply(&mut settings, Vec::new()).await?;
        drop(settings);
        let pushed = match self.push_revocations().await {
            Ok(()) => true,
            Err(error) => {
                self.activity
                    .record(crate::ActivityKind::RevocationsUnpushed {
                        error: error.to_string(),
                        retry: None,
                    });
                false
            }
        };
        let fenced_logs = signed.statement.reset().map_or(0, |reset| {
            reset.fence.iter().map(|scope| scope.logs.len()).sum()
        });
        Ok(crate::ResetReceipt {
            reset: signed.id()?,
            kept: keep,
            follows,
            fenced_logs,
            pushed,
        })
    }
    /// The nodes of every custody identity the registry names that no statement
    /// ended: the servers and custodians a reset keeps unless told otherwise.
    /// Nodes this issuer admitted count before their certificates arrive.
    async fn known_custodians(&self) -> Result<BTreeSet<PeerId>, Failure> {
        let registry = self.registry().await?;
        let membership = self.membership().await?;
        let custody: BTreeSet<PublicKey> = registry
            .records
            .iter()
            .filter(|(_, record)| {
                matches!(
                    record.profile(),
                    Some(rocky_credentials::Profile::Custody { .. })
                )
            })
            .map(|(identity, _)| *identity)
            .collect();
        let settings = self.settings.lock().await;
        let seen = settings
            .authority
            .iter()
            .flat_map(|authority| &authority.seen)
            .filter(|(_, seen)| custody.contains(&seen.identity))
            .map(|(node, _)| *node)
            .filter(|node| !settings.tombstoned(node));
        Ok(custody
            .iter()
            .flat_map(|identity| membership.nodes(identity))
            .map(|linked| linked.node)
            .chain(seen)
            .map(PeerId::of)
            .collect())
    }
    /// Whether an owner reset fenced this device's view, or no other owner
    /// device witnessed it yet; see [`crate::Freshness`]. No owner fence
    /// vouches for an ended device's view; the newest unfollowed reset that
    /// keeps this node fences the rest.
    pub(super) async fn freshness(&self) -> Result<crate::Freshness, Failure> {
        let own = PeerId::of(PrivateKey::from_bytes(&self.vault.get("node")?).public_key());
        let settings = self.settings.lock().await;
        if settings.revocation(&self.path)?.is_some() {
            return Ok(crate::Freshness::LocallyAnchored);
        }
        let baseline = if settings.personal.is_some_and(|device| device.unwitnessed) {
            crate::Freshness::Unwitnessed
        } else {
            crate::Freshness::LocallyAnchored
        };
        drop(settings);
        let revocations = self.replica.revocations().await;
        let fenced = revocations
            .reset_heads()
            .into_iter()
            .filter_map(|id| Some((revocations.get(&id)?, id)))
            .filter(|(signed, _)| {
                signed
                    .statement
                    .reset()
                    .is_some_and(|reset| reset.keep.contains(&own))
            })
            .max_by_key(|(signed, id)| (signed.statement.time, *id))
            .map(|(_, reset)| crate::Freshness::FencedByOwner { reset });
        Ok(fenced.unwrap_or(baseline))
    }
}
