mod app;

use rocky_sdk::{Configuration, Error};

#[tokio::main]
async fn main() -> Result<(), Error> {
    let directory = std::env::args()
        .skip(1)
        .find(|arg| arg != "--create")
        .unwrap_or_else(|| "local/notes".into());
    let configuration = Configuration::new(directory, &app::schema()?)?;
    let database = if std::env::args().any(|arg| arg == "--create") {
        let created = configuration.create().await?;
        println!("Root phrase, keep it offline: {}", created.root.phrase());
        println!(
            "Person key, keep it in the app's secure storage: {}",
            created.owner.phrase()
        );
        created.database
    } else {
        configuration.open().await?
    };
    let changes = database.observe(&["notes"]).await?;
    app::add(&database, "Try Rocky from Rust").await?;
    println!("Committed changes: {:?}", changes.next().await?);
    for note in app::notes(&database).await? {
        println!("{}: {} (done: {})", note.id, note.body, note.done);
    }
    database.close().await
}
