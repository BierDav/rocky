//! Announces this device under the domain's discovery topics on a transport
//! that can, and keeps what it finds as contacts. See decision 0057.
use super::*;
use crate::transport::{Discovery, Found, LOCAL_PATH, PathState, TOPIC_PATH};

/// The topics are derived again at least this often, so the previous one
/// stops being answered once its window ends.
const TOPIC_CHECK: Duration = Duration::from_secs(600);

impl Node {
    fn discovery_time(&self) -> Result<u64, Failure> {
        #[cfg(test)]
        return Ok(now()?.saturating_add(self.discovery_skew.load(Ordering::SeqCst)));
        #[cfg(not(test))]
        now()
    }
    /// The current topic and, within its window, the previous one. None
    /// before an admitted session handed over the root, and none once this
    /// device knows it is revoked or logged out.
    pub(super) async fn discovery_topics(&self) -> Vec<[u8; 32]> {
        if self.ended.lock().await.is_some() || self.replica.private_schema().await.unwrap_or(true)
        {
            return Vec::new();
        }
        let settings = self.settings.lock().await;
        if settings.logged_out || !matches!(settings.revocation(&self.path), Ok(None)) {
            return Vec::new();
        }
        let Ok(time) = self.discovery_time() else {
            return Vec::new();
        };
        match settings.discovery_topics(&self.vault, time) {
            Ok((current, previous)) => std::iter::once(current).chain(previous).collect(),
            Err(_) => Vec::new(),
        }
    }
    /// Announces under the topics this device derives now.
    pub(super) async fn announce(&self, discovery: &dyn Discovery) {
        let topics = self.discovery_topics().await;
        let Ok(node) = self.vault.get("node") else {
            return;
        };
        let node = *PrivateKey::from_bytes(&node).public_key().as_bytes();
        let announced = discovery.announce(node, &topics).await.is_ok() && !topics.is_empty();
        self.announcing.store(announced, Ordering::SeqCst);
    }
    /// Keeps a found contact. It is a hint: the session it leads to
    /// authenticates the peer, and a revoked node is never kept.
    pub(super) async fn learn_found(&self, kind: &str, found: Found) -> Result<(), Failure> {
        let node = PublicKey::from_bytes(&found.node)?;
        {
            let settings = self.settings.lock().await;
            if !settings.would_learn(node, kind, &found.address, found.local) {
                return Ok(());
            }
        }
        let learned = self
            .configure(|settings| {
                settings.learn_found(&self.vault, node, kind, found.address, found.local, now()?)
            })
            .await?;
        if learned {
            self.reset_retry(Some(node));
            self.wake_sync();
        }
        Ok(())
    }
    /// One worker per link whose transport can discover.
    pub(super) async fn discover(self: Arc<Self>, link: Arc<Link>, discovery: Arc<dyn Discovery>) {
        let mut stage = link.stage.subscribe();
        let mut topics = self.topics.subscribe();
        let mut check = tokio::time::interval(TOPIC_CHECK);
        // A local mark describes the network this device is on now.
        let marked = self
            .settings
            .lock()
            .await
            .contact_meta
            .values()
            .any(|meta| meta.local);
        if marked {
            let _ = self
                .configure(|settings| {
                    for meta in settings.contact_meta.values_mut() {
                        meta.local = false;
                    }
                    Ok(())
                })
                .await;
        }
        let mut open = true;
        loop {
            topics.borrow_and_update();
            self.announce(&*discovery).await;
            loop {
                tokio::select! {
                    _ = stage.wait_for(|stage| *stage != Stage::Open) => {
                        // A borrowed transport outlives this service.
                        let _ = discovery.announce([0; 32], &[]).await;
                        self.announcing.store(false, Ordering::SeqCst);
                        return;
                    }
                    found = discovery.found(), if open => match found {
                        Some(found) => {
                            let _ = self.learn_found(link.transport.kind(), found).await;
                        }
                        None => open = false,
                    },
                    _ = topics.changed() => break,
                    _ = check.tick() => break,
                }
            }
        }
    }
    /// A sync or a served session may have changed the revocation set, and
    /// with it the topic.
    pub(super) fn topics_changed(&self) {
        self.topics
            .send_modify(|event| *event = event.wrapping_add(1));
    }
    /// Each discovery path as on, off or unavailable with its reason.
    pub(super) fn discovery_status(&self) -> Value {
        let link = self.current();
        let paths = match link.as_ref().map(|link| (link, &link.discovery)) {
            None => [LOCAL_PATH, TOPIC_PATH]
                .map(|path| (path.to_owned(), PathState::Off))
                .to_vec(),
            Some((link, None)) => [LOCAL_PATH, TOPIC_PATH]
                .map(|path| {
                    let reason =
                        format!("the {} transport has no discovery", link.transport.kind());
                    (path.to_owned(), PathState::Unavailable(reason))
                })
                .to_vec(),
            Some((_, Some(discovery))) => discovery.paths(),
        };
        let paths: serde_json::Map<String, Value> = paths
            .into_iter()
            .map(|(path, state)| {
                let state = match state {
                    PathState::On => json!({"state": "on"}),
                    PathState::Off => json!({"state": "off"}),
                    PathState::Unavailable(reason) => {
                        json!({"state": "unavailable", "reason": reason})
                    }
                };
                (path, state)
            })
            .collect();
        json!({
            "announcing": link.is_some() && self.announcing.load(Ordering::SeqCst),
            "paths": paths,
        })
    }
    /// Stops or resumes the paths that need the application in front.
    pub(super) async fn discovery_foreground(&self, foreground: bool) {
        if let Some(discovery) = self.current().and_then(|link| link.discovery.clone()) {
            discovery.foreground(foreground);
        }
    }
}
