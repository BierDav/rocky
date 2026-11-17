use std::{fs::File, ops::Deref};

pub struct Directory {
    root: tempfile::TempDir,
    _reservation: Option<File>,
}

impl Deref for Directory {
    type Target = tempfile::TempDir;

    fn deref(&self) -> &Self::Target {
        &self.root
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(self.root.path());
    }
}

impl From<tempfile::TempDir> for Directory {
    fn from(root: tempfile::TempDir) -> Self {
        Self {
            root,
            _reservation: None,
        }
    }
}

const RAM_SLOTS: usize = 3;

pub fn small() -> std::io::Result<Directory> {
    if cfg!(target_os = "linux") {
        for slot in 0..RAM_SLOTS {
            let reservation = File::options()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .open(format!("/dev/shm/rocky-fixture-slot-{slot}.lock"));
            if let Ok(reservation) = reservation
                && reservation.try_lock().is_ok()
                && let Ok(root) = tempfile::Builder::new()
                    .prefix("rocky-fixture-")
                    .tempdir_in("/dev/shm")
            {
                return Ok(Directory {
                    root,
                    _reservation: Some(reservation),
                });
            }
        }
    }
    tempfile::tempdir().map(Directory::from)
}
