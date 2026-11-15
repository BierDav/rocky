//! A personal backup archive, verified before a sign-in installs from it.
use super::PersonalSignInError;
use crate::enrollment::Settings;
use rocky_core::Commitment;
use rocky_credentials::{DatabaseContext, DatabaseKind, PersonalAccount, PublicKey};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

/// A signed backup of one owner device of a personal database, verified
/// without extracting it: the signature of the device that made it, every
/// file's digest, the identity record's format and provenance, and the
/// root-signed personal genesis. It carries the genesis descriptor, the
/// membership and tombstones the device held, and in its store the root
/// escrow inventory and the verified log heads; signing in from it adds a
/// device and ends none.
pub struct VerifiedBackup(Box<Verified>);
struct Verified {
    archive: PathBuf,
    id: String,
    node: PublicKey,
    created: u64,
    identity: Settings,
}

impl VerifiedBackup {
    /// Verifies the archive at `archive` (in a browser, a file in Rocky's
    /// storage). Another format version is `UnsupportedFormat`; a shared
    /// database's backup is no personal source.
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
    /// The genesis descriptor every device of the database binds.
    pub fn descriptor(&self) -> Result<Commitment, PersonalSignInError> {
        self.0
            .identity
            .manifest()
            .and_then(|manifest| Ok(manifest.descriptor()?))
            .map_err(PersonalSignInError::Failed)
    }
    /// The owner devices the archived device knew live, itself included.
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
    /// Whether a permanent statement the archive holds ended `node`.
    pub fn tombstones(&self, node: PublicKey) -> bool {
        self.0
            .identity
            .revocations
            .tombstones(self.0.identity.identity, node)
    }
    /// The node key of the device that made the archive.
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
    /// The archive is the personal database `account` locates at `database`:
    /// its root, owner, namespace and location. Another account's archive
    /// holds no source of this one.
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
