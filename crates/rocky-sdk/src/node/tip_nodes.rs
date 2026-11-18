use super::*;
use rocky_credentials::CredentialChain;

pub(crate) const ROLE: &str = "tip-nodes";
const FORMAT: u32 = 2;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedNodes {
    format: u32,
    nodes: Vec<SavedNode>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedNode {
    line: String,
    key: [u8; 32],
    credentials: CredentialChain,
}

impl Node {
    pub(crate) async fn restore_line_signers(&self) -> Result<(), Failure> {
        let Some(bytes) = self.vault.load_bytes(ROLE)? else {
            return Ok(());
        };
        let saved: SavedNodes = serde_json::from_slice(&bytes)?;
        if saved.format != FORMAT {
            return Err("unsupported line node format".into());
        }
        for node in saved.nodes {
            match self
                .replica
                .restore_line_node(rocky_secure::LineNode {
                    key: PrivateKey::from_bytes(&node.key),
                    credentials: node.credentials,
                })
                .await
            {
                Ok(()) | Err(Error::Rejected(_)) => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }
    pub(crate) async fn ensure_tip_node(&self) -> Result<(), Failure> {
        self.tip_node(false).await
    }
    pub(crate) async fn refresh_tip_node(&self) -> Result<(), Failure> {
        self.tip_node(true).await
    }
    async fn tip_node(&self, refresh: bool) -> Result<(), Failure> {
        let mut settings = self.settings.lock().await;
        self.line_node(&mut settings, refresh).await
    }
    pub(crate) async fn refresh_line_node(
        &self,
        settings: &mut super::SettingsGuard<'_>,
    ) -> Result<(), Failure> {
        self.line_node(settings, true).await
    }
    async fn line_node(
        &self,
        settings: &mut super::SettingsGuard<'_>,
        refresh: bool,
    ) -> Result<(), Failure> {
        let needed = self.replica.line_node_needed().await;
        if !needed && !refresh {
            return Ok(());
        }
        let Some((scope, _)) = self.replica.followed_bookmark().await else {
            return Ok(());
        };
        let line = self
            .replica
            .tip_graph(scope)
            .await?
            .line_root(self.replica.followed_tip().await)?;
        let line = HEXLOWER.encode(&line.0);
        let mut saved = match self.vault.load_bytes(ROLE)? {
            Some(bytes) => serde_json::from_slice::<SavedNodes>(&bytes)?,
            None => SavedNodes {
                format: FORMAT,
                nodes: Vec::new(),
            },
        };
        if saved.format != FORMAT {
            return Err("unsupported line node format".into());
        }
        let mut known = self.replica.revocations().await;
        known.merge(
            &settings.revocations,
            settings.publisher,
            self.domain().manifest().app,
            now()?,
        )?;
        for certificate in saved
            .nodes
            .iter()
            .filter_map(|node| node.credentials.certificate())
            .chain(settings.credentials.certificate())
        {
            known.link(certificate.clone())?;
        }
        if settings.lacks(&known)? {
            settings.learn_revocations(&known)?;
            settings.save(&self.path)?;
        }
        self.replica.learn_revocations(&known).await?;
        let grants = |chain: &CredentialChain| -> Result<_, Failure> {
            let held = chain.held().ok_or("line node has no identity capability")?;
            let extended = held.extended()?;
            Ok((held.grants.clone(), extended.partitions, extended.families))
        };
        let indexes: Vec<_> = (0..saved.nodes.len())
            .rev()
            .filter(|index| saved.nodes[*index].line == line)
            .collect();
        for index in indexes {
            let key = PrivateKey::from_bytes(&saved.nodes[index].key);
            if saved.nodes[index].credentials.certificate().is_some() {
                let certificate = saved.nodes[index]
                    .credentials
                    .certificate()
                    .ok_or("saved line node has no certificate")?;
                let Ok(current) = settings.chain_for(
                    &key,
                    settings.identity_links(),
                    certificate.clone(),
                    &self.domain(),
                ) else {
                    continue;
                };
                if grants(&current)? != grants(&saved.nodes[index].credentials)? {
                    saved.nodes[index].credentials = current;
                    self.save_line_nodes(&saved)?;
                }
            }
            let node = rocky_secure::LineNode {
                key,
                credentials: saved.nodes[index].credentials.clone(),
            };
            if self.replica.register_line_node(node).await.is_ok() {
                return Ok(());
            }
        }
        if !needed {
            return Ok(());
        }
        let identity = settings.identity()?;
        let key = PrivateKey::from_bytes(&rocky_platform::entropy::random());
        let links = settings.identity_links();
        let personal = settings
            .personal()?
            .filter(|context| context.owner == settings.identity);
        let certificate = if let Some(context) = personal {
            let certificate = links
                .clone()
                .certify_personal(&*identity, &key, (context.owner, context.descriptor))?
                .0
                .pop()
                .ok_or("missing node certificate")?;
            known.admit_personal(certificate.clone(), &context)?;
            certificate
        } else {
            let membership = known.membership()?;
            if !membership.can_link(settings.identity, |node| {
                known.identity_unlinks(settings.identity, node)
            }) {
                return Err("identity membership limit reached".into());
            }
            let sequence = membership.next_sequence(settings.identity)?;
            let previous = membership
                .head(settings.identity)
                .map(|head| head.certificate);
            let certificate = links
                .clone()
                .certify_member(&*identity, key.public_key(), sequence, previous)?
                .0
                .pop()
                .ok_or("missing node certificate")?;
            known.link(certificate.clone())?;
            certificate
        };
        let credentials = settings.chain_for(&key, links, certificate, &self.domain())?;
        let node = rocky_secure::LineNode { key, credentials };
        saved.nodes.push(SavedNode {
            line,
            key: *node.key.as_bytes(),
            credentials: node.credentials.clone(),
        });
        self.save_line_nodes(&saved)?;
        settings.learn_revocations(&known)?;
        settings.save(&self.path)?;
        self.replica.learn_revocations(&known).await?;
        self.replica.register_line_node(node).await?;
        Ok(())
    }
    fn save_line_nodes(&self, saved: &SavedNodes) -> Result<(), Failure> {
        self.vault.store_bytes(ROLE, &serde_json::to_vec(saved)?)?;
        Ok(())
    }
    pub(crate) async fn try_ensure_tip_node(&self) {
        if let Err(error) = self.ensure_tip_node().await {
            self.activity
                .record_changed(crate::ActivityKind::LineNodeFailed {
                    error: (error.to_string()).to_owned(),
                });
        }
    }
}
