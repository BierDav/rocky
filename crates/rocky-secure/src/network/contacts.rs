//! Contact hints and discovery secrets, available only inside admitted sessions.
use super::*;
use rocky_core::NodeId;
use rocky_credentials::DiscoverySecret;
use std::{future::Future, pin::Pin};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContactHint {
    pub node: PublicKey,
    pub routes: Vec<(String, String)>,
    pub trusted: bool,
}
impl ContactHint {
    pub fn validate(&self) -> Result<(), Error> {
        if self.routes.len() > 4
            || self.routes.iter().any(|(kind, address)| {
                !(1..=32).contains(&kind.len()) || !(1..=2048).contains(&address.len())
            })
        {
            return Err(denied("invalid contact routes"));
        }
        Ok(())
    }
}
#[derive(Clone, Serialize, Deserialize)]
pub enum ContactRequest {
    Handover {
        contacts: Vec<ContactHint>,
    },
    Request {
        node: NodeId,
    },
    Discovery {
        root: Option<Box<DiscoverySecret>>,
        values: Vec<DiscoverySecret>,
        after: Option<rocky_core::Commitment>,
    },
}
#[derive(Clone, Serialize, Deserialize)]
pub enum ContactResponse {
    Handover {
        contacts: Vec<ContactHint>,
    },
    Contact {
        contact: Option<ContactHint>,
    },
    Discovery {
        root: Option<Box<DiscoverySecret>>,
        values: Vec<DiscoverySecret>,
        next: Option<rocky_core::Commitment>,
    },
}
#[derive(Serialize, Deserialize)]
enum Payload {
    Request(ContactRequest),
    Response(ContactResponse),
}
#[derive(Serialize, Deserialize)]
pub(super) struct ContactMessage(#[serde(with = "serde_bytes")] Vec<u8>);
pub type ContactFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ContactResponse, Error>> + Send + 'a>>;
/// Local contact and secret storage, independent of the replica and transport.
pub trait ContactHandler: Send + Sync {
    fn answer(&self, peer: PublicKey, request: ContactRequest) -> ContactFuture<'_>;
}
fn validate(request: &ContactRequest) -> Result<(), Error> {
    match request {
        ContactRequest::Handover { contacts } => {
            if contacts.len() > 16 {
                return Err(denied("contact handover limit"));
            }
            for contact in contacts {
                contact.validate()?;
            }
        }
        ContactRequest::Discovery { root, values, .. }
            if values.len() > 32
                || root.as_ref().is_some_and(|root| root.statement.is_some())
                || values.iter().any(|value| value.statement.is_none()) =>
        {
            return Err(denied("discovery exchange limit"));
        }
        _ => {}
    }
    Ok(())
}
fn handover_count(payload: &Payload) -> usize {
    match payload {
        Payload::Request(ContactRequest::Handover { contacts })
        | Payload::Response(ContactResponse::Handover { contacts }) => contacts.len(),
        _ => 0,
    }
}
impl NativeSession {
    async fn send_contact(&mut self, payload: Payload) -> Result<(), Error> {
        let count = handover_count(&payload);
        if self.contact_handed + count > 16 {
            return Err(denied("contact handover limit"));
        }
        self.contact_handed += count;
        let sealed = rocky_credentials::seal_discovery(self.contact_key, &encode(&payload)?)
            .map_err(credential)?;
        self.send(Message::Contacts(ContactMessage(sealed))).await
    }
    fn open_contact(&mut self, message: ContactMessage) -> Result<Payload, Error> {
        let bytes =
            rocky_credentials::open_discovery(self.contact_key, &message.0).map_err(credential)?;
        let payload = decode(&bytes)?;
        let count = handover_count(&payload);
        if self.contact_received + count > 16 {
            return Err(denied("contact handover limit"));
        }
        self.contact_received += count;
        Ok(payload)
    }

    pub async fn exchange_contacts(
        &mut self,
        request: ContactRequest,
    ) -> Result<ContactResponse, Error> {
        validate(&request)?;
        if matches!(request, ContactRequest::Request { .. }) {
            if self.contact_requests >= 8 {
                return Err(denied("contact request limit"));
            }
            self.contact_requests += 1;
        }
        self.send_contact(Payload::Request(request.clone())).await?;
        let Message::Contacts(message) = self.receive().await? else {
            return Err(denied("unexpected contact response"));
        };
        let Payload::Response(response) = self.open_contact(message)? else {
            return Err(denied("unexpected contact response"));
        };
        match (&request, &response) {
            (ContactRequest::Handover { .. }, ContactResponse::Handover { contacts }) => {
                validate(&ContactRequest::Handover {
                    contacts: contacts.clone(),
                })?
            }
            (ContactRequest::Discovery { .. }, ContactResponse::Discovery { root, values, .. }) => {
                validate(&ContactRequest::Discovery {
                    root: root.clone(),
                    values: values.clone(),
                    after: None,
                })?
            }
            (ContactRequest::Request { node }, ContactResponse::Contact { contact }) => {
                if let Some(contact) = contact {
                    contact.validate()?;
                    if crate::secure_node_id(contact.node) != *node {
                        return Err(denied("contact node ID mismatch"));
                    }
                }
            }
            _ => return Err(denied("unexpected contact response")),
        }
        Ok(response)
    }
    /// Answers one contact request inside an admitted session.
    pub(super) async fn answer_contact<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        handler: &dyn ContactHandler,
        message: ContactMessage,
    ) -> Result<(), Error> {
        let Payload::Request(request) = self.open_contact(message)? else {
            return Err(denied("unexpected contact request"));
        };
        replica.admit_chain(self.peer, &self.credentials).await?;
        validate(&request)?;
        if matches!(request, ContactRequest::Request { .. }) {
            if self.contact_requests >= 8 {
                return Err(denied("contact request limit"));
            }
            self.contact_requests += 1;
        }
        let response = handler.answer(self.peer, request).await?;
        replica.admit_chain(self.peer, &self.credentials).await?;
        self.send_contact(Payload::Response(response)).await
    }
    /// Serves the SDK mesh metadata alongside rows and optional commands.
    pub async fn serve_with_contacts<A: rocky_core::RowCommandApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        coordinator: Option<&dyn rocky_core::Coordinator>,
        handler: &dyn ContactHandler,
    ) -> Result<(), Error> {
        loop {
            let message = match self.answered().await {
                Err(Error::Io(error)) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                    return Ok(());
                }
                other => other?,
            };
            match message {
                Message::Contacts(message) => {
                    self.answer_contact(replica, handler, message).await?
                }
                Message::Commands(message) => {
                    let coordinator =
                        coordinator.ok_or_else(|| denied("command coordinator unavailable"))?;
                    self.answer_command(replica, coordinator, message).await?;
                }
                message => self.answer(replica, message).await?,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::group_sync_measurement::fixture::Fixture;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Unknown(AtomicUsize);
    impl ContactHandler for Unknown {
        fn answer(&self, _peer: PublicKey, _request: ContactRequest) -> ContactFuture<'_> {
            self.0.fetch_add(1, Ordering::Relaxed);
            Box::pin(async { Ok(ContactResponse::Contact { contact: None }) })
        }
    }

    #[tokio::test]
    async fn the_server_refuses_a_ninth_request_before_calling_the_handler() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = Fixture::new(1);
        let client_key = PrivateKey::new();
        let server_key = PrivateKey::new();
        let app = rocky_core::TypedCommands::new(
            fixture.app(),
            Default::default(),
            Default::default(),
            None,
        )
        .unwrap();
        let manifest = app.manifest();
        let signed =
            rocky_credentials::SignedManifest::sign(&manifest, &fixture.publisher).unwrap();
        let config = |node: &PrivateKey| {
            let user = PrivateKey::new();
            let root = rocky_credentials::Authority {
                publisher: &fixture.publisher,
                manifest: manifest.hash(),
            }
            .issue_partition_terms(
                user.public_key(),
                vec![rocky_core::ResourceGrant {
                    scope: fixture.scope("products", 0),
                    rows: rocky_core::RowSelection::All,
                    rights: [rocky_core::Right::Read].into(),
                    fields: Default::default(),
                }],
                vec![],
                (1, None),
                (now().unwrap(), 3600, 5),
            )
            .unwrap();
            SecureConfig {
                publisher: fixture.publisher.public_key(),
                manifest: signed.clone(),
                revocations: fixture.revocations.clone(),
                node: node.clone(),
                credentials: CredentialChain(vec![root])
                    .complete(&user, node.public_key())
                    .unwrap(),
                keys: vec![],
            }
        };
        let client =
            EncryptedReplica::open(dir.path().join("client"), app.clone(), config(&client_key))
                .await
                .unwrap();
        let server = EncryptedReplica::open(dir.path().join("server"), app, config(&server_key))
            .await
            .unwrap();
        let client_chain = client.inner.lock().await.config.credentials.clone();
        let server_chain = server.inner.lock().await.config.credentials.clone();
        let (mut c, mut s) = super::link::testing::sessions(
            (&client, client_key, client_chain),
            (&server, server_key, server_chain),
        )
        .await
        .unwrap();
        let handler = Unknown(AtomicUsize::new(0));
        let ((), error) = tokio::join!(
            async {
                for _ in 0..8 {
                    assert!(matches!(
                        c.exchange_contacts(ContactRequest::Request {
                            node: NodeId([7; 16])
                        })
                        .await
                        .unwrap(),
                        ContactResponse::Contact { contact: None }
                    ));
                }
                // Bypass the client's counter to exercise the receiver's bound.
                c.send_contact(Payload::Request(ContactRequest::Request {
                    node: NodeId([7; 16]),
                }))
                .await
                .unwrap();
            },
            s.serve_with_contacts(&server, None, &handler)
        );
        assert!(
            error
                .unwrap_err()
                .to_string()
                .contains("contact request limit")
        );
        assert_eq!(handler.0.load(Ordering::Relaxed), 8);
    }
}
