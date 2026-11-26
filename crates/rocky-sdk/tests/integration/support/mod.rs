#[path = "../../../src/test_directory.rs"]
mod directory;
pub mod secrets;
pub mod snapshot;
pub use directory::Directory;

pub fn small_directory() -> std::io::Result<Directory> {
    directory::small()
}

pub fn peerless_ok(
    result: Result<rocky_sdk::status::SyncReport, rocky_sdk::Error>,
) -> Result<Option<rocky_sdk::status::SyncReport>, rocky_sdk::Error> {
    match result {
        Ok(report) => Ok(Some(report)),
        Err(error)
            if matches!(
                error.kind(),
                rocky_sdk::ErrorKind::Service(rocky_sdk::ServiceError::NoPeerReached(report))
                    if report.peers().is_empty()
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}
