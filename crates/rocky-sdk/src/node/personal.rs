use super::*;
use crate::personal::bootstrap::{Metadata, Page};
use rocky_credentials::{
    PersonalAct, PersonalAudience, PersonalAuthorization, PersonalChallenge,
    PersonalCustodyRenewal, PersonalError, PersonalHello, PersonalPublication,
};

const NONCE_LIFETIME: u64 = 300;
const MAX_NONCES: usize = 256;
const NONCE_TAG: &str = "rocky.personal-nonce.v1";
const PAGE_ESCROW_BYTES: usize = 1024 * 1024;

#[cfg(test)]
pub(crate) static FORGED_SCHEMA: std::sync::Mutex<Vec<(std::path::PathBuf, crate::Schema)>> =
    std::sync::Mutex::new(Vec::new());

pub(crate) struct Nonces {
    key: [u8; 32],
    spent: BTreeMap<[u8; 32], (u64, Vec<PersonalAct>)>,
}
impl Default for Nonces {
    fn default() -> Self {
        Self {
            key: rocky_platform::entropy::random(),
            spent: BTreeMap::new(),
        }
    }
}
impl Nonces {
    fn tag(&self, head: &[u8]) -> [u8; 16] {
        let digest = rocky_core::commitment(NONCE_TAG, &[self.key.as_slice(), head].concat());
        let mut tag = [0; 16];
        tag.copy_from_slice(&digest[..16]);
        tag
    }
    fn issue(&self, time: u64) -> [u8; 32] {
        let mut nonce = [0; 32];
        nonce[..8].copy_from_slice(&time.to_be_bytes());
        nonce[8..16].copy_from_slice(&rocky_platform::entropy::random::<8>());
        let tag = self.tag(&nonce[..16]);
        nonce[16..].copy_from_slice(&tag);
        nonce
    }
    fn spend(&mut self, nonce: &[u8; 32], act: PersonalAct, time: u64) -> Result<(), Failure> {
        let unknown = || refused("the personal nonce is unknown, expired or spent");
        let (head, tag) = nonce.split_at(16);
        let mut issued = [0; 8];
        issued.copy_from_slice(&head[..8]);
        let issued = u64::from_be_bytes(issued);
        if issued > time || time - issued >= NONCE_LIFETIME || self.tag(head) != tag {
            return Err(unknown());
        }
        self.spent
            .retain(|_, (issued, _)| time.saturating_sub(*issued) < NONCE_LIFETIME);
        if !self.spent.contains_key(nonce) && self.spent.len() >= MAX_NONCES {
            return Err(refused("too many personal sign-ins in progress"));
        }
        let (_, used) = self.spent.entry(*nonce).or_insert((issued, Vec::new()));
        if used.contains(&act) {
            return Err(unknown());
        }
        used.push(act);
        Ok(())
    }
}

impl Node {
    async fn personal_account(
        &self,
        prf: crate::VerifiedPasskeyPrf,
    ) -> Result<crate::PersonalAccount, Failure> {
        let settings = self.settings.lock().await;
        let rocky_credentials::DatabaseKind::Personal {
            owner, namespace, ..
        } = settings.kind()?
        else {
            return Err(refused("this database is not a personal one"));
        };
        let account = crate::PersonalAccount::derive(prf, &namespace)
            .map_err(|_| crate::PersonalSignInError::PrfUnsupported)?;
        if account.locator().publisher != settings.publisher || account.locator().owner != owner {
            return Err(crate::PersonalSignInError::WrongLocalAccount.into());
        }
        Ok(account)
    }
    pub(super) async fn revoke_personal_device(
        &self,
        node: rocky_credentials::PeerId,
        prf: crate::VerifiedPasskeyPrf,
    ) -> Result<crate::DeviceRevocationReceipt, Failure> {
        let account = self.personal_account(prf).await?;
        let identity = Arc::new(account.identity());
        drop(account);
        crate::identities::supplying(Some(identity), self.unlink(node)).await
    }
    pub(super) async fn reset_personal(
        &self,
        prf: crate::VerifiedPasskeyPrf,
        keep: crate::ResetKeep,
    ) -> Result<crate::ResetReceipt, Failure> {
        let account = self.personal_account(prf).await?;
        let (root, _) = account.into_parts();
        self.sign_out_everywhere_else(&Root::held(Arc::new(root)), keep)
            .await
    }
    pub(super) async fn personal_device_notices(
        &self,
    ) -> Result<Vec<crate::PersonalDeviceNotice>, Failure> {
        self.absorb_revocations(None).await?;
        let settings = self.settings.lock().await;
        let context = settings
            .personal()?
            .ok_or_else(|| refused("this database is not a personal one"))?;
        Ok(settings
            .authority
            .as_ref()
            .and_then(|a| a.trusted_nodes.get(&context.owner))
            .into_iter()
            .flat_map(|nodes| nodes.iter())
            .map(|(node, notice)| crate::PersonalDeviceNotice {
                scope: context.database.scope,
                owner: context.owner,
                node: rocky_credentials::PeerId::of(*node),
                first_observed: notice.linked,
                read: notice.read,
                acknowledged: notice.acknowledged,
                unlinked: notice.unlinked,
            })
            .collect())
    }
    pub(super) async fn personal_device_notice(
        &self,
        node: rocky_credentials::PeerId,
        acknowledge: bool,
    ) -> Result<(), Failure> {
        self.personal_device_notices().await?;
        self.configure(|settings| {
            let owner = settings
                .personal()?
                .ok_or_else(|| refused("this database is not a personal one"))?
                .owner;
            let notice = settings
                .authority
                .as_mut()
                .and_then(|a| a.trusted_nodes.get_mut(&owner))
                .and_then(|nodes| {
                    nodes
                        .iter_mut()
                        .find(|(key, _)| rocky_credentials::PeerId::of(**key) == node)
                        .map(|(_, notice)| notice)
                })
                .ok_or_else(|| refused("no notice for this device"))?;
            notice.read = true;
            notice.acknowledged |= acknowledge;
            Ok(())
        })
        .await?;
        self.refresh_trusted_watch().await?;
        self.observations.touch();
        Ok(())
    }

    async fn personal_audience(&self) -> Result<(Settings, PersonalAudience), Failure> {
        let settings: Settings = (**self.settings.lock().await).clone();
        let context = settings
            .personal()?
            .ok_or_else(|| refused("this database is not a personal one"))?;
        if settings.logged_out || self.revocation_notice.revoked(&settings)?.is_some() {
            return Err(refused("this device was revoked or logged out"));
        }
        Ok((
            settings,
            PersonalAudience {
                database: context.database,
                owner: context.owner,
                descriptor: context.descriptor,
                node: Settings::node_key(&self.vault)?.public_key(),
            },
        ))
    }
    fn personal_ended(known: &RevocationSet, owner: PublicKey, node: PublicKey) -> bool {
        known.tombstones(owner, node) || known.unfenced().ends_node_of(owner, node, false)
    }
    fn personal_node(
        known: &RevocationSet,
        audience: &PersonalAudience,
        authorization: &PersonalAuthorization,
    ) -> Result<PublicKey, Failure> {
        let node = authorization.node();
        if Self::personal_ended(known, audience.owner, node) {
            return Err(crate::enrollment::personal_refusal(
                PersonalError::Tombstoned {
                    node: rocky_credentials::PeerId::of(node),
                },
            ));
        }
        Ok(node)
    }
    async fn personal_known(&self, settings: &Settings) -> Result<RevocationSet, Failure> {
        let mut known = settings.revocations.clone();
        known.merge(
            &self.replica.revocations().await,
            settings.publisher,
            self.domain().manifest().app,
            now()?,
        )?;
        if let Some(certificate) = settings.credentials.certificate() {
            known.link(certificate.clone())?;
        }
        Ok(known)
    }

    pub(super) async fn answer_personal_hello(
        &self,
        hello: PersonalHello,
    ) -> Result<PersonalChallenge, Failure> {
        let (settings, audience) = self.personal_audience().await?;
        let nonce = self
            .personal_nonces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .issue(now()?);
        Ok(PersonalChallenge::answer(
            &hello,
            audience.database,
            &settings.manifest()?,
            &Settings::node_key(&self.vault)?,
            nonce,
        )?)
    }

    pub(super) async fn answer_personal_bootstrap(
        &self,
        authorization: PersonalAuthorization,
    ) -> Result<rocky_credentials::PersonalAnswer, Failure> {
        let (mut settings, audience) = self.personal_audience().await?;
        authorization.verify(&audience)?;
        let PersonalAct::Bootstrap { page } = authorization.act() else {
            return Err(refused("the authorization is not for a bootstrap"));
        };
        self.personal_nonces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .spend(&authorization.nonce(), authorization.act(), now()?)?;
        let mut known = self.personal_known(&settings).await?;
        let node = authorization.node();
        let ended = Self::personal_ended(&known, audience.owner, node);
        if ended && page != 0 {
            return Err(crate::enrollment::personal_refusal(
                PersonalError::Tombstoned {
                    node: rocky_credentials::PeerId::of(node),
                },
            ));
        }
        let (mut escrow, held) = if ended {
            (Vec::new(), std::collections::BTreeSet::new())
        } else {
            self.replica.escrow_inventory().await?
        };
        escrow.retain(|(key_epoch, _)| key_epoch.is_some());
        escrow.sort_by_key(|(slot, record)| (*slot, record.tag()));
        let mut pages = vec![Vec::new()];
        let mut bytes = 0;
        for record in escrow {
            let size = rocky_credentials::encode(&record)?.len();
            if bytes + size > PAGE_ESCROW_BYTES
                && pages.last().is_some_and(|page: &Vec<_>| !page.is_empty())
            {
                pages.push(Vec::new());
                bytes = 0;
            }
            bytes += size;
            pages
                .last_mut()
                .ok_or("missing bootstrap page")?
                .push(record);
        }
        let total = u32::try_from(pages.len()).map_err(|_| "bootstrap pages")?;
        let index = usize::try_from(page).map_err(|_| "bootstrap page")?;
        let escrow = pages
            .into_iter()
            .nth(index)
            .ok_or_else(|| refused("no such bootstrap page"))?;
        let metadata = if page == 0 {
            settings.bootstrap = Some(match self.enrollment_bootstrap().await {
                Ok(bootstrap) => bootstrap,
                Err(_) => settings.schema_bootstrap()?,
            });
            if !ended {
                let key = Settings::node_key(&self.vault)?;
                crate::reset::admit_after_resets(&mut known, &key, audience.database, node)?;
            }
            let record = crate::tip_schemas::Record::new(
                &settings.schema,
                settings.generation,
                settings.engine,
            );
            #[cfg(test)]
            let record = {
                let mut forged = FORGED_SCHEMA.lock().unwrap_or_else(PoisonError::into_inner);
                match forged.iter().position(|(path, _)| *path == self.path) {
                    Some(index) => crate::tip_schemas::Record::new(
                        &forged.remove(index).1,
                        settings.generation,
                        settings.engine,
                    ),
                    None => record,
                }
            };
            Some(Box::new(Metadata {
                record,
                bootstrap: settings.schema_bootstrap()?,
                ttl: settings.ttl,
                grant: settings.owner_grant(audience.owner)?.clone(),
                responder: settings.credentials.clone(),
                revocations: known,
                groups: settings
                    .groups
                    .values()
                    .map(|record| crate::groups::GroupRecord {
                        epochs: Vec::new(),
                        ..record.clone()
                    })
                    .collect(),
                epoch_statements: settings.epoch_statements.clone(),
                held,
                naming: if ended {
                    None
                } else {
                    match self.replica.custody_naming_escrow().await? {
                        Some(record) => Some(record),
                        None => match crate::escrow::seal_naming(&settings, &self.vault)? {
                            Some(record) => Some(record),
                            None => settings.naming_escrow.clone(),
                        },
                    }
                },
            }))
        } else {
            None
        };
        let page = Page {
            metadata,
            escrow,
            pages: total,
        };
        Ok(authorization.answer(&rocky_credentials::encode(&page)?)?)
    }

    pub(super) async fn accept_personal(
        &self,
        publication: PersonalPublication,
    ) -> Result<RevocationSet, Failure> {
        let publication_id = publication.check()?;
        let (_, audience) = self.personal_audience().await?;
        publication.authorization.verify(&audience)?;
        self.personal_nonces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .spend(
                &publication.authorization.nonce(),
                publication.authorization.act(),
                now()?,
            )?;
        let mut current = self.settings.lock().await;
        let context = current
            .personal()?
            .ok_or_else(|| refused("this database is not a personal one"))?;
        let mut known = self.personal_known(&current).await?;
        let node = Self::personal_node(&known, &audience, &publication.authorization)?;
        let certificate = publication.certificate;
        rocky_credentials::CredentialChain(vec![
            current.owner_grant(context.owner)?.clone(),
            certificate.clone(),
        ])
        .historic_provenance_with_families(
            current.publisher,
            node,
            &self.domain().families,
        )?;
        let added = known.admit_personal(certificate, &context)?;
        if added {
            let mut draft = current.clone();
            draft.learn_revocations(&known)?;
            current.commit(draft, &self.path)?;
            tracing::debug!(
                node = %node.to_hex(),
                publication = %data_encoding::HEXLOWER.encode(&publication_id),
                "personal device certificate recorded"
            );
        }
        self.replica.learn_revocations(&known).await?;
        drop(current);
        if added {
            self.observations.touch();
            self.topics_changed();
            self.wake_sync();
        }
        Ok(known)
    }

    pub(super) async fn witnessed_by(&self, peer: PublicKey) -> Result<(), Failure> {
        let mut current = self.settings.lock().await;
        let Some(device) = current.personal.filter(|device| device.unwitnessed) else {
            return Ok(());
        };
        let rocky_credentials::DatabaseKind::Personal { owner, .. } = current.kind()? else {
            return Ok(());
        };
        let known = self.personal_known(&current).await?;
        if peer == Settings::node_key(&self.vault)?.public_key()
            || Self::personal_ended(&known, owner, peer)
            || !known.personal_devices(owner)?.contains(&peer)
        {
            return Ok(());
        }
        let mut draft = current.clone();
        draft.personal = Some(crate::personal::Device {
            unwitnessed: false,
            ..device
        });
        current.commit(draft, &self.path)?;
        tracing::debug!(peer = %peer.to_hex(), "an owner device witnessed this device");
        Ok(())
    }

    pub(super) async fn accept_custody_renewal(
        &self,
        renewal: PersonalCustodyRenewal,
    ) -> Result<RevocationSet, Failure> {
        let renewed = renewal.renewed()?.clone();
        let (_, audience) = self.personal_audience().await?;
        renewal.authorization.verify(&audience)?;
        self.personal_nonces
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .spend(
                &renewal.authorization.nonce(),
                renewal.authorization.act(),
                now()?,
            )?;
        let mut current = self.settings.lock().await;
        if !matches!(current.profile, rocky_credentials::Profile::Custody { .. }) {
            return Err(refused("only a custodian takes a custody renewal"));
        }
        let known = self.personal_known(&current).await?;
        Self::personal_node(&known, &audience, &renewal.authorization)?;
        let links = current.identity_links();
        let (Some(prior), Some(owner_grant)) = (links.0.last(), links.0.first()) else {
            return Err(refused("this custodian holds no custody link"));
        };
        let prior = &prior.capability;
        let same = renewed.capability.subject == prior.subject
            && renewed.capability.kind == prior.kind
            && renewed.capability.grants == prior.grants
            && renewed.capability.delegations == prior.delegations
            && renewed.capability.extended()? == prior.extended()?
            && renewed.issuer == audience.owner
            && renewal
                .chain
                .0
                .first()
                .map(|grant| grant.id())
                .transpose()?
                == Some(owner_grant.id()?)
            && prior.grants.iter().all(|grant| {
                grant.rights == [rocky_core::Right::Custody].into() && grant.fields.is_none()
            });
        if !same {
            return Err(refused("the renewal is not this custodian's custody grant"));
        }
        let time = now()?;
        if renewed.capability.expires <= prior.expires.max(time) {
            return Err(refused("the renewal does not extend the custody grant"));
        }
        let links = rocky_credentials::CredentialChain(vec![owner_grant.clone(), renewed]);
        let chain = current.certify(&self.vault, links, &self.domain())?;
        let renewed = crate::enrollment::Renewed {
            contacted: None,
            revocations: known.clone(),
            chain,
            keys: Vec::new(),
            epoch_statements: BTreeMap::new(),
            groups: Vec::new(),
            profile: None,
            issuers: Vec::new(),
            claims: None,
            naming: None,
        };
        current.adopt(&self.path, &self.vault, &renewed)?;
        self.apply(&mut current, Vec::new()).await?;
        drop(current);
        tracing::debug!("custody grant renewed under the owner");
        self.wake_sync();
        Ok(known)
    }
}

#[cfg(test)]
mod tests;
