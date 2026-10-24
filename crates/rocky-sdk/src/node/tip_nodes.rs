//! A node per line: an author log continues along one line of `continue`
//! seals, so a device on another line writes under another node of its
//! identity. Their keys and certificates stay in the secret store.
use super::*;
use rocky_credentials::CredentialChain;

pub(crate) const ROLE: &str = "tip-nodes";
const FORMAT: u32 = 1;

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedNodes {
    format: u32,
    /// In enrollment order; the newest node of a line is tried first.
    nodes: Vec<SavedNode>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SavedNode {
    line: String,
    key: [u8; 32],
    credentials: CredentialChain,
}

impl Settings {
    /// A stored identity's one number is its first node's; another node
    /// takes the next free number of the ledger grants also draw from.
    fn reserve_line_author(&mut self) -> Result<u32, Failure> {
        let minting = self
            .minting
            .as_mut()
            .ok_or("no free author number for another node")?;
        let author = minting.next;
        if !minting.range.contains(author) {
            return Err("no free author number for another node".into());
        }
        minting.next = author.checked_add(1).ok_or("author numbers exhausted")?;
        self.author_high_water = self.author_high_water.max(minting.next);
        Ok(author)
    }
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
            // Expired or superseded authors remain unusable; restoring a signer
            // never selects it for writes or changes its persisted line.
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
    /// Registers a node for the followed line when writes need one: a
    /// saved node of the line, else a newly certified one. Readers hold no
    /// author range and write nothing.
    pub(crate) async fn ensure_tip_node(&self) -> Result<(), Failure> {
        self.tip_node(false).await
    }
    /// After a schema switch a registered line node is certified again, so
    /// its grants name the tables the installed definition adds.
    pub(crate) async fn refresh_tip_node(&self) -> Result<(), Failure> {
        self.tip_node(true).await
    }
    async fn tip_node(&self, refresh: bool) -> Result<(), Failure> {
        // Serializes number reservations with grants and renewals.
        let mut settings = self.settings.lock().await;
        self.line_node(&mut settings, refresh).await
    }
    /// [`Self::refresh_tip_node`] for a caller that holds the settings.
    pub(crate) async fn refresh_line_node(
        &self,
        settings: &mut Shared<Settings>,
    ) -> Result<(), Failure> {
        self.line_node(settings, true).await
    }
    async fn line_node(
        &self,
        settings: &mut Shared<Settings>,
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
        let Some(range) = settings.identity_links().authors()? else {
            return Ok(());
        };
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
        let identity = settings.identity(&self.vault)?;
        let grants =
            |chain: &CredentialChain| chain.0.last().map(|leaf| leaf.capability.grants.clone());
        let indexes: Vec<_> = (0..saved.nodes.len())
            .rev()
            .filter(|index| saved.nodes[*index].line == line)
            .collect();
        for index in indexes {
            let key = PrivateKey::from_bytes(&saved.nodes[index].key);
            // A schema change may grant new tables: the same node and
            // number are certified again for the installed definition.
            if let Some(author) = saved.nodes[index]
                .credentials
                .authors()?
                .and_then(|range| range.single())
            {
                let current = settings.certify_node(
                    &*identity,
                    settings.identity_links(),
                    &self.domain(),
                    (key.public_key(), author),
                )?;
                if grants(&current) != grants(&saved.nodes[index].credentials) {
                    saved.nodes[index].credentials = current;
                    self.vault.store_bytes(ROLE, &serde_json::to_vec(&saved)?)?;
                }
            }
            let node = rocky_secure::LineNode {
                key,
                credentials: saved.nodes[index].credentials.clone(),
            };
            // A saved node that crossed a fork or expired is not reused.
            if self.replica.register_line_node(node).await.is_ok() {
                return Ok(());
            }
        }
        // The genesis line writes under the original node.
        if !needed {
            return Ok(());
        }
        let author = if settings.stored_identity {
            let mut draft = (**settings).clone();
            let author = draft.reserve_line_author()?;
            draft.save(&self.path)?;
            *settings = Shared::new(draft);
            author
        } else {
            identity.reserve_author(settings.node_numbers(range))?
        };
        let key = PrivateKey::new();
        let credentials = settings.certify_node(
            &*identity,
            settings.identity_links(),
            &self.domain(),
            (key.public_key(), author),
        )?;
        let node = rocky_secure::LineNode { key, credentials };
        saved.nodes.push(SavedNode {
            line,
            key: *node.key.as_bytes(),
            credentials: node.credentials.clone(),
        });
        // Saved before registration, so a retry reuses this certificate.
        self.vault.store_bytes(ROLE, &serde_json::to_vec(&saved)?)?;
        self.replica.register_line_node(node).await?;
        Ok(())
    }
    /// Background callers keep their result; writes report a missing node.
    pub(crate) async fn try_ensure_tip_node(&self) {
        if let Err(error) = self.ensure_tip_node().await {
            self.activity
                .record_changed("line_node_failed", json!({"error": error.to_string()}));
        }
    }
}
