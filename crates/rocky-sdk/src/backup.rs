//! Signed backup archives of one database: a coherent copy of its file,
//! and its identity record, which holds no key. An archive is restored only into a fresh
//! signing identity enrolled with an issuer. See
//! `docs/runtime/backup-and-restore.md`.
use rocky_credentials::PublicKey;
use serde::{Deserialize, Serialize};

/// Written before a restore enrolls: the restored file's predecessor.
pub(crate) const MARKER: &str = "restore.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Entry {
    pub name: String,
    pub length: u64,
    pub sha256: String,
}
/// What an archive holds and who made it. Its exact bytes are signed.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Inventory {
    pub format: u32,
    pub created: u64,
    pub publisher: PublicKey,
    /// The domain's scope as the call contract spells it.
    pub scope: serde_json::Value,
    /// The hash of the signed manifest the file is installed with.
    pub manifest: String,
    /// The node key of the device that made it; it signs the archive.
    pub node: PublicKey,
    /// The database revision the copy holds.
    pub revision: u64,
    pub files: Vec<Entry>,
}
/// The restore or recovery in progress in a directory. It goes once the
/// restored file is rebound and holds its keys.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Marker {
    pub predecessor: PublicKey,
    pub archive: String,
    /// How far a recovery got; `None` for a restore.
    pub recovery: Option<Recovery>,
}
/// A recovery's progress. Its identity is saved first, so resuming never
/// takes another author slot.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Recovery {
    /// The identity is saved; the archive's files may not be in place.
    Placing,
    /// The files are in place; the store opens without its keys, takes its
    /// tables, then its keys.
    Placed,
    /// The store holds its tables and keys; the root has yet to revoke the
    /// lost trusted identities.
    Keyed,
}

// Archives are files a user moves elsewhere: natively streamed from disk; in
// a browser, whole files in Rocky's storage (`rocky_platform::fs`).
pub(crate) use archive::{open, write};
mod archive {
    use super::*;
    use crate::{Failure, enrollment::Settings};
    use data_encoding::HEXLOWER;
    use rocky_credentials::{PrivateKey, Signature};
    use sha2::{Digest, Sha256};
    use std::{
        io::{Read, Write},
        path::{Path, PathBuf},
    };

    /// Reads an archive or a staged file.
    fn reader(path: &Path) -> Result<impl Read, Failure> {
        #[cfg(not(target_family = "wasm"))]
        return Ok(std::io::BufReader::new(std::fs::File::open(path)?));
        #[cfg(target_family = "wasm")]
        return Ok(std::io::Cursor::new(rocky_platform::fs::read(path)?));
    }
    /// A file written whole, durable on `finish`. A browser buffers it, and
    /// puts a database where SQLite opens it.
    struct Sink {
        #[cfg(not(target_family = "wasm"))]
        out: std::io::BufWriter<std::fs::File>,
        #[cfg(target_family = "wasm")]
        out: (PathBuf, Vec<u8>),
    }
    impl Sink {
        fn create(path: &Path) -> Result<Self, Failure> {
            #[cfg(not(target_family = "wasm"))]
            return Ok(Self {
                out: std::io::BufWriter::new(std::fs::File::create(path)?),
            });
            #[cfg(target_family = "wasm")]
            return Ok(Self {
                out: (path.to_path_buf(), Vec::new()),
            });
        }
        fn finish(self, database: bool) -> Result<(), Failure> {
            #[cfg(not(target_family = "wasm"))]
            {
                let _ = database;
                self.out
                    .into_inner()
                    .map_err(|error| error.into_error())?
                    .sync_all()?;
            }
            #[cfg(target_family = "wasm")]
            {
                let (path, bytes) = self.out;
                if database {
                    rocky_platform::fs::write_database(path, &bytes)?;
                } else {
                    rocky_platform::fs::write(path, bytes)?;
                }
            }
            Ok(())
        }
    }
    impl Write for Sink {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            #[cfg(not(target_family = "wasm"))]
            return self.out.write(bytes);
            #[cfg(target_family = "wasm")]
            return self.out.1.write(bytes);
        }
        fn flush(&mut self) -> std::io::Result<()> {
            #[cfg(not(target_family = "wasm"))]
            return self.out.flush();
            #[cfg(target_family = "wasm")]
            return Ok(());
        }
    }

    const MAGIC: &[u8] = b"rocky.backup.v2\n";
    const FORMAT: u32 = 2;
    const MAX_HEADER: usize = 256 * 1024;
    /// The files an archive carries, in order. The database is required.
    const DATABASE: &str = "rocky.sqlite";
    const IDENTITY: &str = "identity.json";

    fn label(inventory: &[u8]) -> Vec<u8> {
        [b"rocky.backup.v2".as_slice(), inventory].concat()
    }
    fn digest(path: &Path) -> Result<(u64, String), Failure> {
        let mut reader = reader(path)?;
        let (mut hash, mut length, mut buffer) = (Sha256::new(), 0u64, vec![0; 64 * 1024]);
        loop {
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                return Ok((length, HEXLOWER.encode(&hash.finalize())));
            }
            hash.update(&buffer[..read]);
            length += read as u64;
        }
    }

    /// Writes the archive of `directory`'s staged files to `target` and signs
    /// it with `node`. `staged` names each file's copy; it is written to a
    /// temporary file first and renamed into place.
    pub(crate) fn write(
        target: &Path,
        staged: &[(&str, PathBuf)],
        settings: &Settings,
        scope: serde_json::Value,
        revision: u64,
        node: &PrivateKey,
        created: u64,
    ) -> Result<Inventory, Failure> {
        let mut files = Vec::new();
        for (name, path) in staged {
            let (length, sha256) = digest(path)?;
            files.push(Entry {
                name: (*name).to_string(),
                length,
                sha256,
            });
        }
        let inventory = Inventory {
            format: FORMAT,
            created,
            publisher: settings.publisher,
            scope,
            manifest: HEXLOWER.encode(&settings.manifest()?.verify(settings.publisher)?.hash()),
            node: node.public_key(),
            revision,
            files,
        };
        let header = serde_json::to_vec(&inventory)?;
        let signature = node.sign(&label(&header));
        let partial = target.with_extension("partial");
        let mut out = Sink::create(&partial)?;
        out.write_all(MAGIC)?;
        out.write_all(&(header.len() as u32).to_le_bytes())?;
        out.write_all(&header)?;
        out.write_all(&signature.to_bytes())?;
        for (_, path) in staged {
            std::io::copy(&mut reader(path)?, &mut out)?;
        }
        out.finish(false)?;
        rocky_platform::fs::rename(&partial, target)?;
        Ok(inventory)
    }

    /// A verified archive: its inventory, the identity record it carries and
    /// where its files were extracted.
    pub(crate) struct Opened {
        pub inventory: Inventory,
        pub identity: Settings,
        pub files: Vec<(String, PathBuf)>,
        /// A digest of the signed inventory, naming this archive.
        pub id: String,
    }
    /// Reads and verifies `archive`, extracting its files into `staging`.
    /// The signature must be the exporter's, whose credentials the publisher
    /// issued for the manifest; every file must match its digest.
    pub(crate) fn open(archive: &Path, staging: &Path) -> Result<Opened, Failure> {
        let mut input = reader(archive)?;
        let mut magic = vec![0; MAGIC.len()];
        input.read_exact(&mut magic)?;
        if magic != MAGIC {
            return Err("not a Rocky backup archive".into());
        }
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > MAX_HEADER {
            return Err("backup inventory quota".into());
        }
        let mut header = vec![0; length];
        input.read_exact(&mut header)?;
        let mut signature = [0; 64];
        input.read_exact(&mut signature)?;
        let inventory: Inventory = serde_json::from_slice(&header)?;
        if inventory.format != FORMAT {
            return Err("unsupported backup archive format".into());
        }
        if !inventory
            .node
            .verify(&label(&header), &Signature::from_bytes(&signature))
        {
            return Err("the backup archive's signature does not verify".into());
        }
        let names: Vec<&str> = inventory
            .files
            .iter()
            .map(|entry| entry.name.as_str())
            .collect();
        if names.first() != Some(&IDENTITY) || names.get(1) != Some(&DATABASE) || names.len() != 2 {
            return Err("the backup archive holds unexpected files".into());
        }
        rocky_platform::fs::create_dir_all(staging)?;
        let mut files = Vec::new();
        for entry in &inventory.files {
            let path = staging.join(format!("{}.restore", entry.name));
            let mut out = Sink::create(&path)?;
            std::io::copy(&mut (&mut input).take(entry.length), &mut out)?;
            out.finish(entry.name == DATABASE)?;
            if digest(&path)? != (entry.length, entry.sha256.clone()) {
                return Err(format!("{} differs from the backup inventory", entry.name).into());
            }
            files.push((entry.name.clone(), path));
        }
        if input.read(&mut [0])? != 0 {
            return Err("the backup archive has trailing bytes".into());
        }
        let identity: Settings = serde_json::from_slice(&rocky_platform::fs::read(&files[0].1)?)?;
        let manifest = identity.manifest()?.verify(inventory.publisher)?;
        if identity.publisher != inventory.publisher
            || HEXLOWER.encode(&manifest.hash()) != inventory.manifest
        {
            return Err("the backup identity differs from its inventory".into());
        }
        identity
            .credentials
            .provenance(inventory.publisher, inventory.node)?;
        Ok(Opened {
            id: HEXLOWER.encode(&rocky_core::commitment("rocky.backup.id.v1", &header)),
            inventory,
            identity,
            files,
        })
    }
}
