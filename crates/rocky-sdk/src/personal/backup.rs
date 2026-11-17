use super::PersonalSignInError;
use crate::enrollment::Settings;
use rocky_core::Commitment;
use rocky_credentials::{DatabaseContext, DatabaseKind, PersonalAccount, PublicKey};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

pub struct VerifiedBackup(Box<Verified>);
struct Verified {
    archive: PathBuf,
    id: String,
    node: PublicKey,
    created: u64,
    identity: Settings,
}

impl VerifiedBackup {
    pub fn open(archive: impl Into<PathBuf>) -> Result<Self, PersonalSignInError> {
        let archive = archive.into();
        let (inventory, identity, id) = crate::backup::inspect(&archive).map_err(|error| {
            if error.is::<crate::backup::Unsupported>() {
                PersonalSignInError::UnsupportedFormat
            } else {
                PersonalSignInError::Failed(error)
            }
        })?;
        if !identity
            .kind()
            .map_err(PersonalSignInError::Failed)?
            .is_personal()
        {
            return Err(PersonalSignInError::Failed(
                crate::enrollment::personal_refusal(rocky_credentials::PersonalError::ModeMismatch),
            ));
        }
        Ok(Self(Box::new(Verified {
            archive,
            id,
            node: inventory.node,
            created: inventory.created,
            identity,
        })))
    }
    pub fn descriptor(&self) -> Result<Commitment, PersonalSignInError> {
        self.0
            .identity
            .manifest()
            .and_then(|manifest| Ok(manifest.descriptor()?))
            .map_err(PersonalSignInError::Failed)
    }
    pub fn devices(&self) -> Result<BTreeSet<PublicKey>, PersonalSignInError> {
        let mut known = self.0.identity.revocations.clone();
        if let Some(certificate) = self.0.identity.credentials.certificate() {
            known
                .link(certificate.clone())
                .map_err(|error| PersonalSignInError::Failed(error.into()))?;
        }
        let DatabaseKind::Personal { owner, .. } = self
            .0
            .identity
            .kind()
            .map_err(PersonalSignInError::Failed)?
        else {
            return Ok(BTreeSet::new());
        };
        known
            .personal_devices(owner)
            .map_err(|error| PersonalSignInError::Failed(error.into()))
    }
    pub fn tombstones(&self, node: PublicKey) -> bool {
        self.0
            .identity
            .revocations
            .tombstones(self.0.identity.identity, node)
    }
    pub fn node(&self) -> PublicKey {
        self.0.node
    }
    pub fn created(&self) -> u64 {
        self.0.created
    }
    pub(crate) fn archive(&self) -> &Path {
        &self.0.archive
    }
    pub(crate) fn id(&self) -> &str {
        &self.0.id
    }
    pub(crate) fn check(
        &self,
        account: &PersonalAccount,
        database: &DatabaseContext,
    ) -> Result<(), PersonalSignInError> {
        let locator = account.locator();
        let identity = &self.0.identity;
        let matches = identity.publisher == locator.publisher
            && identity
                .manifest()
                .ok()
                .and_then(|genesis| {
                    genesis
                        .verify_personal(locator.publisher, locator.owner, account.namespace())
                        .ok()
                })
                .is_some_and(|manifest| manifest.app == database.scope.app)
            && identity
                .database_context()
                .is_ok_and(|context| context == *database);
        if !matches {
            return Err(PersonalSignInError::SourceUnavailable);
        }
        Ok(())
    }
}
impl std::fmt::Debug for VerifiedBackup {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VerifiedBackup")
            .field("archive", &self.0.archive)
            .field("node", &self.0.node)
            .finish_non_exhaustive()
    }
}
