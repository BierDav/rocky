use super::*;
use rocky_credentials::MAX_LIVE_REVOCATIONS;
use std::collections::BTreeSet;

const STATEMENT_ID_BYTES: usize = 32;
const IDS_PAGE_BYTES: usize = 32 * 1024;
const IDS_PAGE: usize = IDS_PAGE_BYTES / STATEMENT_ID_BYTES;
const STATEMENTS_PAGE: usize = 32;
const MAX_IDS: usize = 4 * MAX_LIVE_REVOCATIONS;

#[derive(Serialize, Deserialize)]
pub(super) enum RevocationMessage {
    Exchange,
    Certificates {
        certificates: Vec<rocky_credentials::SignedCapability>,
        last: bool,
    },
    Digest {
        #[serde(with = "serde_bytes")]
        digest: Commitment,
    },
    Ids {
        #[serde(with = "serde_bytes")]
        ids: Vec<u8>,
        last: bool,
    },
    Statements {
        statements: Vec<Statement>,
        last: bool,
    },
}
#[derive(Serialize, Deserialize, Clone)]
pub(super) struct Statement(#[serde(with = "serde_bytes")] Vec<u8>);

impl NativeSession {
    pub async fn refresh_revocations<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
    ) -> Result<(), Error> {
        self.send(RevocationMessage::Exchange).await?;
        self.acknowledged("authority exchange acknowledgement")
            .await?;
        self.refresh_authority(replica, true).await
    }
    pub(super) async fn refresh_authority<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        initiator: bool,
    ) -> Result<(), Error> {
        let own = replica
            .inner
            .lock(crate::replica::Class::Background)
            .await
            .config
            .credentials
            .clone();
        if let Some(statement) = self.exchange_revocations(replica, &own, initiator).await? {
            return Err(Error::Application(Box::new(Revoked(Box::new(statement)))));
        }
        let held = replica.revocations().await;
        if held.ends_chain(&own).map_err(credential)?
            || held.ends_chain(&self.credentials).map_err(credential)?
        {
            return Err(denied("membership certificate is void"));
        }
        Ok(())
    }
    pub(super) async fn exchange_revocations<A: RowApplication>(
        &mut self,
        replica: &EncryptedReplica<A>,
        own: &CredentialChain,
        initiator: bool,
    ) -> Result<Option<SignedRevocation>, Error> {
        let held = replica.revocations().await;
        let time = now()?;
        let digest = held.exchange_digest(time).map_err(credential)?;
        let RevocationMessage::Digest { digest: theirs } = self
            .swap(initiator, RevocationMessage::Digest { digest })
            .await?
        else {
            return Err(denied("unexpected revocation digest"));
        };
        if theirs == digest {
            return Ok(None);
        }
        let certificates: Vec<_> = held.certificates().cloned().collect();
        let mut pages = certificates.chunks(STATEMENTS_PAGE);
        let (mut sent, mut received) = (false, false);
        let mut learned_links = RevocationSet::new();
        let mut received_links = BTreeSet::new();
        while !(sent && received) {
            let page = pages.next().unwrap_or_default();
            sent = pages.len() == 0;
            let RevocationMessage::Certificates { certificates, last } = self
                .swap(
                    initiator,
                    RevocationMessage::Certificates {
                        certificates: page.to_vec(),
                        last: sent,
                    },
                )
                .await?
            else {
                return Err(denied("unexpected membership certificates"));
            };
            if (received && !certificates.is_empty()) || certificates.len() > STATEMENTS_PAGE {
                return Err(denied("membership certificates outside the exchange"));
            }
            for certificate in certificates {
                let id = certificate.id().map_err(credential)?;
                if !received_links.insert(id) {
                    return Err(denied("duplicate membership certificate"));
                }
                if !held.holds_certificate(&id) {
                    learned_links.link(certificate).map_err(credential)?;
                }
            }
            received |= last;
        }
        replica.learn_revocations(&learned_links).await?;
        let ours: Vec<Commitment> = held
            .statements()
            .filter(|signed| signed.live(time))
            .map(SignedRevocation::id)
            .collect::<Result<_, _>>()
            .map_err(credential)?;
        let mut named = BTreeSet::new();
        let mut pages = ours.chunks(IDS_PAGE);
        let (mut sent, mut received) = (false, false);
        while !(sent && received) {
            let page = pages.next().unwrap_or_default();
            sent = pages.len() == 0;
            let RevocationMessage::Ids { ids, last } = self
                .swap(
                    initiator,
                    RevocationMessage::Ids {
                        ids: page.concat(),
                        last: sent,
                    },
                )
                .await?
            else {
                return Err(denied("unexpected revocation IDs"));
            };
            if (received && !ids.is_empty())
                || ids.len() % STATEMENT_ID_BYTES != 0
                || ids.len() / STATEMENT_ID_BYTES > IDS_PAGE
            {
                return Err(denied("revocation IDs outside the exchange"));
            }
            named.extend(ids.as_chunks::<STATEMENT_ID_BYTES>().0.iter().copied());
            if named.len() > MAX_IDS {
                return Err(denied("revocation IDs outside the exchange"));
            }
            received |= last;
        }
        let lacking: Vec<Statement> = held
            .statements()
            .filter(|signed| signed.live(time))
            .zip(&ours)
            .filter(|(_, id)| !named.contains(*id))
            .map(|(signed, _)| signed.to_bytes().map(Statement))
            .collect::<Result<_, _>>()
            .map_err(credential)?;
        let mut wanted: BTreeSet<Commitment> = named
            .into_iter()
            .filter(|id| held.get(id).is_none())
            .collect();
        let mut learned = Vec::new();
        let mut rest = lacking.as_slice();
        let (mut sent, mut received) = (false, false);
        while !(sent && received) {
            let (page, after) = rest.split_at(page_len(rest, STATEMENTS_PAGE)?);
            rest = after;
            sent = rest.is_empty();
            let RevocationMessage::Statements { statements, last } = self
                .swap(
                    initiator,
                    RevocationMessage::Statements {
                        statements: page.to_vec(),
                        last: sent,
                    },
                )
                .await?
            else {
                return Err(denied("unexpected revocation statements"));
            };
            if (received && !statements.is_empty()) || statements.len() > STATEMENTS_PAGE {
                return Err(denied("revocation statements outside the exchange"));
            }
            for Statement(bytes) in statements {
                let signed = SignedRevocation::from_bytes(&bytes).map_err(credential)?;
                let named_as_lacking_and_unseen = wanted.remove(&signed.id().map_err(credential)?);
                if !named_as_lacking_and_unseen {
                    return Err(denied("revocation statements outside the exchange"));
                }
                learned.push(signed);
            }
            received |= last;
        }
        replica.learn_session_revocations(learned, own).await
    }
    async fn swap(
        &mut self,
        initiator: bool,
        message: RevocationMessage,
    ) -> Result<RevocationMessage, Error> {
        if initiator {
            self.send(message).await?;
            self.reply::<RevocationMessage>().await
        } else {
            let reply = self.reply::<RevocationMessage>().await?;
            self.send(message).await?;
            Ok(reply)
        }
    }
}

#[cfg(test)]
mod page_tests {
    use super::*;

    #[wasm_bindgen_test::wasm_bindgen_test(unsupported = test)]
    fn a_full_page_of_statement_ids_fits_one_signed_frame() {
        crate::network::budgets::fits_one_frame(
            RevocationMessage::Ids {
                ids: vec![255; IDS_PAGE_BYTES],
                last: false,
            }
            .into(),
        );
    }
}
