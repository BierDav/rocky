use super::*;
use rocky_secure::network::{ContactFuture, ContactHandler, ContactRequest, ContactResponse};

impl ContactHandler for Node {
    fn answer(&self, _peer: PublicKey, request: ContactRequest) -> ContactFuture<'_> {
        Box::pin(async move {
            self.answer_contacts(request)
                .await
                .map_err(rocky_secure::Error::Application)
        })
    }
}
impl Node {
    async fn answer_contacts(&self, request: ContactRequest) -> Result<ContactResponse, Failure> {
        // Contact exchange must preserve the pending epoch refresh from the handshake.
        self.absorb_revocations(None).await?;
        let mut settings = self.settings.lock().await;
        let mut changed = false;
        let response = match request {
            ContactRequest::Handover { contacts } => {
                for contact in contacts {
                    changed |= settings.learn_contact(&self.vault, contact, "handover", now()?)?;
                }
                let pending = self.handover.lock().unwrap_or_else(PoisonError::into_inner);
                let mut bytes = 0;
                let contacts = pending
                    .iter()
                    .filter_map(|node| settings.contact_hint(*node))
                    .filter(|contact| {
                        let size = contact
                            .routes
                            .iter()
                            .map(|(kind, route)| kind.len() + route.len())
                            .sum::<usize>()
                            + 256;
                        if bytes + size > 96 * 1024 {
                            return false;
                        }
                        bytes += size;
                        true
                    })
                    .take(16)
                    .collect();
                ContactResponse::Handover { contacts }
            }
            ContactRequest::Request { node } => {
                let contact = settings
                    .peers
                    .keys()
                    .find(|key| secure_node_id(**key) == node)
                    .and_then(|key| settings.contact_hint(*key));
                ContactResponse::Contact { contact }
            }
            ContactRequest::Discovery {
                root,
                values,
                after,
            } => settings.discovery_exchange(&self.vault, root, values, after)?,
        };
        if changed {
            settings.save(&self.path)?;
        }
        Ok(response)
    }
    /// Asks admitted peers for a route; the later session still proves its key.
    pub(super) async fn request_contact(
        &self,
        transport: &dyn Transport,
        id: rocky_core::NodeId,
    ) -> Result<Value, Failure> {
        let settings = self.settings.lock().await.clone();
        for (peer, contact) in self.dialed(&settings) {
            if self
                .retries
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get(&peer)
                .is_some_and(|state| !state.retry.ready())
            {
                continue;
            }
            let link = match self.request_link(transport, peer, &contact).await {
                Ok(link) => link,
                Err(_) => continue,
            };
            let service = self
                .current()
                .ok_or(ServiceError::NetworkRequired("contact"))?;
            let _short = service.live.short(peer);
            let mut session = link.open().await?;
            let response = async {
                let response = session
                    .exchange_contacts(ContactRequest::Request { node: id })
                    .await?;
                session.close().await?;
                link.drain().await?;
                Ok::<_, Failure>(response)
            }
            .await;
            let response = response?;
            if let ContactResponse::Contact {
                contact: Some(contact),
            } = response
            {
                if secure_node_id(contact.node) != id {
                    return Err(refused("answered contact does not name the requested node"));
                }
                let node = contact.node;
                self.configure(|settings| {
                    settings.learn_contact(&self.vault, contact, "request", now()?)
                })
                .await?;
                self.wake_sync();
                return Ok(json!({"node": node.to_hex()}));
            }
        }
        Ok(json!({"node": null}))
    }
}

impl Node {
    pub(super) async fn resolve_saved_contacts(&self, transport: &dyn Transport) {
        let contacts = self.settings.lock().await.unresolved_contacts();
        let ready: Vec<_> = {
            let mut retries = self
                .resolution_retries
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            retries.retain(|id, _| contacts.iter().any(|(peer, _)| peer == id));
            contacts
                .into_iter()
                .filter(|(id, _)| retries.entry(*id).or_default().ready())
                .take(crate::enrollment::MAX_DIALED)
                .collect()
        };
        let saved = self.settings.lock().await.clone();
        for (id, _) in &ready {
            let attempt = self.observations.begin(
                HEXLOWER.encode(id.as_bytes()),
                crate::AttemptOperation::ResolveContact,
                crate::AttemptStage::ResolveRoute,
            );
            match saved.resolve_saved_route(transport, *id).await {
                Ok(Some(node)) => {
                    self.settings.lock().await.bind_saved_contact(node);
                    attempt.finish(None);
                }
                Ok(None) => {
                    attempt.finish_empty();
                }
                Err(error) => {
                    attempt.finish(Some(crate::status::failure(
                        &error,
                        &crate::AttemptStage::ResolveRoute,
                    )));
                }
            }
        }
        for (id, _) in ready
            .iter()
            .filter(|(_, contact)| contact.routes.is_empty())
        {
            if !self
                .settings
                .lock()
                .await
                .unresolved_contacts()
                .iter()
                .any(|(peer, _)| peer == id)
            {
                continue;
            }
            let mut bytes = [0; 16];
            bytes.copy_from_slice(&id.as_bytes()[..16]);
            let attempt = self.observations.begin(
                HEXLOWER.encode(id.as_bytes()),
                crate::AttemptOperation::ResolveContact,
                crate::AttemptStage::ResolveRoute,
            );
            match self
                .request_contact(transport, rocky_core::NodeId(bytes))
                .await
            {
                Ok(value) => {
                    if value["node"].is_null() {
                        attempt.finish_empty();
                    } else {
                        attempt.finish(None);
                    }
                }
                Err(error) => {
                    attempt.finish(Some(crate::status::failure(
                        &error,
                        &crate::AttemptStage::ResolveRoute,
                    )));
                }
            }
        }
        let unresolved = self.settings.lock().await.unresolved_contacts();
        let mut retries = self
            .resolution_retries
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        for (id, _) in ready {
            if unresolved.iter().any(|(peer, _)| peer == &id) {
                retries.entry(id).or_default().failed();
            } else {
                retries.remove(&id);
            }
        }
    }
}
