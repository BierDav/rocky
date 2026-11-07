//! The revocation exchange that ends every handshake: both digests, then on
//! a difference the live statement IDs and the statements each side lacks,
//! in lock step: the initiator sends a page, the acceptor answers with one.
use super::*;
use rocky_credentials::MAX_LIVE_REVOCATIONS;
use std::collections::BTreeSet;

/// Statement IDs per page: 32 KiB, which the session's array encoding of
/// message bytes doubles, well inside one frame.
const IDS_PAGE: usize = 1024;
/// Statements per page: each holds at most 2,048 bytes.
const STATEMENTS_PAGE: usize = 32;
/// IDs a peer may name: two partitioned issuers near the live bound may
/// together exceed it, so the merge, not this, bounds the set.
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
    /// One page of the sender's live statement IDs, concatenated.
    Ids {
        #[serde(with = "serde_bytes")]
        ids: Vec<u8>,
        last: bool,
    },
    /// Statements the receiver lacks, each in its own encoding.
    Statements {
        statements: Vec<Statement>,
        last: bool,
    },
}
#[derive(Serialize, Deserialize)]
pub(super) struct Statement(#[serde(with = "serde_bytes")] Vec<u8>);

impl NativeSession {
    /// Refreshes replicated membership and revocations on an existing link.
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
    /// Leaves `replica` holding every statement the peer sent but one that
    /// revokes `own`, which it returns. Both sides run it; `initiator`
    /// decides who speaks first in each step.
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
                if !learned_links.link(certificate).map_err(credential)? {
                    return Err(denied("duplicate membership certificate"));
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
            if (received && !ids.is_empty()) || ids.len() % 32 != 0 || ids.len() / 32 > IDS_PAGE {
                return Err(denied("revocation IDs outside the exchange"));
            }
            named.extend(ids.as_chunks::<32>().0.iter().copied());
            if named.len() > MAX_IDS {
                return Err(denied("revocation IDs outside the exchange"));
            }
            received |= last;
        }
        let lacking: Vec<Vec<u8>> = held
            .statements()
            .filter(|signed| signed.live(time))
            .zip(&ours)
            .filter(|(_, id)| !named.contains(*id))
            .map(|(signed, _)| signed.to_bytes())
            .collect::<Result<_, _>>()
            .map_err(credential)?;
        let mut wanted: BTreeSet<Commitment> = named
            .into_iter()
            .filter(|id| held.get(id).is_none())
            .collect();
        let mut learned = Vec::new();
        let mut pages = lacking.chunks(STATEMENTS_PAGE);
        let (mut sent, mut received) = (false, false);
        while !(sent && received) {
            let page = pages.next().unwrap_or_default();
            sent = pages.len() == 0;
            let RevocationMessage::Statements { statements, last } = self
                .swap(
                    initiator,
                    RevocationMessage::Statements {
                        statements: page.iter().cloned().map(Statement).collect(),
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
                // Only a statement this side named as lacking, once.
                if !wanted.remove(&signed.id().map_err(credential)?) {
                    return Err(denied("revocation statements outside the exchange"));
                }
                learned.push(signed);
            }
            received |= last;
        }
        replica.learn_session_revocations(learned, own).await
    }
    /// One lock-step exchange: the initiator sends first.
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
