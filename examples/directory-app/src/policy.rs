use crate::*;
use std::collections::BTreeSet;
impl Directory {
    pub fn decode(data: &[u8]) -> Result<Self, Invalid> {
        if data.len() > 256 * 1024 {
            return Err(Invalid);
        }
        let value: Self = serde_json::from_slice(data).map_err(|_| Invalid)?;
        if value.count() > QUOTA || value.users.iter().any(|u| principal(u.key) != u.principal) {
            return Err(Invalid);
        }
        Ok(value)
    }
    pub fn encode(&self) -> Result<Vec<u8>, Invalid> {
        let data = serde_json::to_vec(self).map_err(|_| Invalid)?;
        if self.count() > QUOTA || data.len() > 256 * 1024 {
            return Err(Invalid);
        }
        Ok(data)
    }
    fn count(&self) -> usize {
        self.users.len()
            + self.deployments.len()
            + self.nodes.len()
            + self.delegations.len()
            + self.ownership.len()
            + self
                .artifacts
                .iter()
                .map(|a| a.app_grants.len() + a.keys.len() + a.pending.len())
                .sum::<usize>()
            + self.deleted_keys.len()
            + self
                .deletions
                .iter()
                .map(|d| {
                    1 + d.revoked_nodes.len()
                        + d.revoked_grants.len()
                        + d.rotate_keys.len()
                        + d.resolve_pending.len()
                })
                .sum::<usize>()
    }
    fn has_key(&self, key: Commitment) -> bool {
        self.deleted_keys.contains(&key)
            || self.users.iter().any(|u| u.key == key)
            || self.deployments.iter().any(|d| d.key == key)
            || self.nodes.iter().any(|n| n.key == key)
    }
    fn user(&self, caller: PrincipalId) -> bool {
        self.users.iter().any(|u| u.principal == caller)
    }
    fn allowed(
        &self,
        caller: PrincipalId,
        owner: PrincipalId,
        right: DirectoryRight,
        time: u64,
    ) -> bool {
        self.user(owner)
            && (caller == owner
                || self.delegations.iter().any(|g| {
                    g.owner == owner
                        && g.subject == caller
                        && g.rights.contains(&right)
                        && self.active(g, time)
                }))
    }
    fn active(&self, grant: &Delegation, time: u64) -> bool {
        if time < grant.not_before || time >= grant.expires {
            return false;
        }
        let mut parent = grant.parent;
        for _ in 0..8 {
            let Some(id) = parent else {
                return true;
            };
            let Some(g) = self.delegations.iter().find(|g| g.id == id) else {
                return false;
            };
            if time < g.not_before || time >= g.expires {
                return false;
            }
            parent = g.parent;
        }
        false
    }
    /// Learned directory revocations reject future sessions; app grants remain app-owned.
    pub fn registered_node(&self, key: Commitment) -> bool {
        !self.deleted_keys.contains(&key)
            && self
                .nodes
                .iter()
                .any(|n| n.key == key && self.user(n.owner))
    }
}
impl DirectoryApp {
    pub fn transition(
        &self,
        mut directory: Directory,
        caller: PrincipalId,
        transition: Transition,
    ) -> Result<Directory, Invalid> {
        match transition {
            Transition::RegisterUser { key } => {
                if caller != self.bootstrap_registrar || directory.has_key(key) {
                    return Err(Invalid);
                }
                directory.users.push(User {
                    key,
                    principal: principal(key),
                });
            }
            Transition::RegisterDeployment { key, owner, time } => {
                if !directory.allowed(caller, owner, DirectoryRight::RegisterDeployment, time)
                    || directory.has_key(key)
                {
                    return Err(Invalid);
                }
                directory.deployments.push(Deployment { key, owner });
            }
            Transition::RegisterNode {
                key,
                owner,
                deployment,
                time,
            } => {
                if !directory.allowed(caller, owner, DirectoryRight::RegisterNode, time)
                    || directory.has_key(key)
                    || deployment.is_some_and(|key| {
                        !directory
                            .deployments
                            .iter()
                            .any(|d| d.key == key && d.owner == owner)
                    })
                {
                    return Err(Invalid);
                }
                directory.nodes.push(Node {
                    key,
                    owner,
                    deployment,
                });
            }
            Transition::Delegate { grant, time } => {
                if directory.delegations.iter().any(|g| g.id == grant.id)
                    || directory.deleted_keys.contains(&grant.id)
                    || grant.id == [0; 32]
                    || grant.rights.is_empty()
                    || grant.depth > 7
                    || grant.not_before > time
                    || grant.expires <= time
                    || grant.expires - grant.not_before > 366 * 86400
                    || !directory.user(grant.owner)
                    || !directory.user(grant.subject)
                {
                    return Err(Invalid);
                }
                if let Some(id) = grant.parent {
                    let parent = directory
                        .delegations
                        .iter()
                        .find(|g| g.id == id)
                        .ok_or(Invalid)?;
                    if parent.subject != caller
                        || parent.owner != grant.owner
                        || !parent.rights.contains(&DirectoryRight::Delegate)
                        || !directory.active(parent, time)
                        || grant.depth >= parent.depth
                        || !grant.rights.is_subset(&parent.rights)
                        || grant.not_before < parent.not_before
                        || grant.expires > parent.expires
                    {
                        return Err(Invalid);
                    }
                } else if caller != grant.owner {
                    return Err(Invalid);
                }
                directory.delegations.push(grant);
            }
            Transition::RevokeDelegation { id } => {
                let grant = directory
                    .delegations
                    .iter()
                    .find(|g| g.id == id)
                    .ok_or(Invalid)?;
                if caller != grant.owner {
                    return Err(Invalid);
                }
                let mut revoked = BTreeSet::from([id]);
                loop {
                    let old = revoked.len();
                    for g in &directory.delegations {
                        if g.parent.is_some_and(|p| revoked.contains(&p)) {
                            revoked.insert(g.id);
                        }
                    }
                    if old == revoked.len() {
                        break;
                    }
                }
                directory.delegations.retain(|g| !revoked.contains(&g.id));
                // Tombstones prohibit grant ID reuse as well as credential key reuse.
                directory.deleted_keys.extend(revoked);
            }
            Transition::TrackArtifacts(artifacts) => {
                if caller != artifacts.owner
                    || !directory.user(caller)
                    || artifacts.app_grants.len() > 64
                    || artifacts.keys.len() > 64
                    || artifacts.pending.len() > 64
                {
                    return Err(Invalid);
                }
                if let Some(old) = directory.artifacts.iter_mut().find(|a| a.owner == caller) {
                    for grant in artifacts.app_grants {
                        if !old.app_grants.contains(&grant) {
                            old.app_grants.push(grant);
                        }
                    }
                    for key in artifacts.keys {
                        if !old.keys.contains(&key) {
                            old.keys.push(key);
                        }
                    }
                    for pending in artifacts.pending {
                        if !old.pending.contains(&pending) {
                            old.pending.push(pending);
                        }
                    }
                } else {
                    directory.artifacts.push(artifacts);
                }
            }
            Transition::DeleteUser { disposition } => {
                if !directory.user(caller) {
                    return Err(Invalid);
                }
                let owned: Vec<_> = directory
                    .ownership
                    .iter()
                    .filter(|o| o.owner == caller)
                    .map(|o| o.scope)
                    .collect();
                if disposition.len()!=owned.len() || disposition.iter().any(|(scope,action)| !owned.contains(scope) || matches!(action,Disposition::Transfer(to) if *to==caller || !directory.user(*to)))
                    || disposition.iter().map(|(s,_)| (s.mesh,s.app,s.domain)).collect::<BTreeSet<_>>().len()!=disposition.len() { return Err(Invalid); }
                for (scope, action) in &disposition {
                    match action {
                        Disposition::Transfer(to) => {
                            for o in &mut directory.ownership {
                                if o.scope == *scope {
                                    o.owner = *to;
                                }
                            }
                        }
                        Disposition::Retire => directory.ownership.retain(|o| o.scope != *scope),
                    }
                }
                let nodes: Vec<_> = directory
                    .nodes
                    .iter()
                    .filter(|n| n.owner == caller)
                    .map(|n| n.key)
                    .collect();
                let mut grants: Vec<_> = directory
                    .delegations
                    .iter()
                    .filter(|g| g.owner == caller || g.subject == caller)
                    .map(|g| g.id)
                    .collect();
                loop {
                    let old = grants.len();
                    for g in &directory.delegations {
                        if g.parent.is_some_and(|id| grants.contains(&id))
                            && !grants.contains(&g.id)
                        {
                            grants.push(g.id);
                        }
                    }
                    if old == grants.len() {
                        break;
                    }
                }
                let artifacts = directory
                    .artifacts
                    .iter()
                    .find(|a| a.owner == caller)
                    .cloned()
                    .unwrap_or(Artifacts {
                        owner: caller,
                        app_grants: vec![],
                        keys: vec![],
                        pending: vec![],
                    });
                directory.deleted_keys.extend(nodes.iter().copied());
                directory.deleted_keys.extend(
                    directory
                        .deployments
                        .iter()
                        .filter(|d| d.owner == caller)
                        .map(|d| d.key),
                );
                directory.deleted_keys.extend(
                    directory
                        .users
                        .iter()
                        .filter(|u| u.principal == caller)
                        .map(|u| u.key),
                );
                directory.deleted_keys.extend(grants.iter().copied());
                directory.users.retain(|u| u.principal != caller);
                directory.nodes.retain(|n| n.owner != caller);
                directory.deployments.retain(|d| d.owner != caller);
                directory.delegations.retain(|g| !grants.contains(&g.id));
                directory.artifacts.retain(|a| a.owner != caller);
                grants.extend(artifacts.app_grants);
                directory.deletions.push(Deletion {
                    user: caller,
                    revoked_nodes: nodes,
                    revoked_grants: grants,
                    rotate_keys: artifacts.keys,
                    resolve_pending: artifacts.pending,
                    disposition,
                });
            }
        }
        directory.encode()?;
        Ok(directory)
    }
    pub fn manifest(&self) -> ApplicationManifest {
        let mut m = ApplicationManifest {
            publisher: self.publisher,
            name: "directory".into(),
            app: ApplicationManifest::app_id(self.publisher, "directory"),
            schema: 1,
            projection: 1,
            handler: 1,
            entities: vec![],
            fields: vec![],
            methods: vec![],
            operations: vec![],
            domains: vec![DomainRule {
                domain: self.domain,
                author_log: AuthorLogScope::Domain,
                rule: "directory identities only; no application data rights".into(),
            }],
        };
        m.entities.push(DefinitionItem {
            id: m.item_id("entity", "Directory"),
            name: "Directory".into(),
            shape:
                "bounded snapshot; users/deployments/nodes; scoped delegation; deletion disposition"
                    .into(),
        });
        m.methods.push(DefinitionItem {
            id: m.item_id("method", COMMAND),
            name: COMMAND.into(),
            shape: format!(
                "v1 predecessor transition; registrar={:?}; ownership={:?}",
                self.bootstrap_registrar, self.initial_ownership
            ),
        });
        m
    }
}
