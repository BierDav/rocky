#[path = "../../examples/notes/app.rs"]
mod app;

use crate::common::Failure;
use rocky_sdk::Configuration;

#[tokio::test]
async fn the_notes_example_reads_back_the_note_it_wrote() -> Result<(), Failure> {
    let directory = tempfile::tempdir()?;
    let database = Configuration::new(directory.path().join("notes"), &app::schema()?)?
        .create()
        .await?
        .database;
    let id = app::add(&database, "Try Rocky from Rust").await?;
    let notes = app::notes(&database).await?;
    assert_eq!(notes.len(), 1);
    assert_eq!(notes[0].id, id);
    assert_eq!(notes[0].body, "Try Rocky from Rust");
    assert!(!notes[0].done);
    database.close().await?;
    Ok(())
}
