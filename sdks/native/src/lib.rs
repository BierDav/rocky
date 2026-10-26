//! Rocky DB's official Rust application SDK.
//!
//! Add `rocky = { path = "path/to/rocky-db/sdks/native" }` to your dependencies.
//! Disable default features for storage without iroh. No ORM or native binding
//! generator is needed. Run async SDK methods inside a Tokio runtime.
//!
//! ```no_run
//! use rocky::{Affinity, Column, Configuration, Failure, Schema, Table, params};
//!
//! # async fn application() -> Result<(), Failure> {
//! let schema = Schema::builder("notes", 1)
//!     .table(Table::mergeable("notes")
//!         .column(Column::new("body", Affinity::Text).required()))
//!     .build()?;
//! let configuration = Configuration::new("notes-data", &schema)?;
//! let database = configuration.create().await?.database;
//! let mut transaction = database.transaction().await?;
//! let id = transaction.next_id("notes", params![]).await?;
//! transaction.execute("INSERT INTO notes(id,body) VALUES(?,?)", params![id, "Hello Rocky"])
//!     .await?;
//! transaction.commit().await?;
//! let mut rows = database.query("SELECT body FROM notes WHERE id=?", params![id]).await?;
//! if let Some(row) = rows.next().await? {
//!     println!("{}", row.get::<String>("body")?);
//! }
//! drop(rows);
//! database.close().await?;
//! # Ok(()) }
//! ```
//!
//! Reuse configuration with `open().await` for an existing installation or
//! `join(invitation).await` to enroll. Networking is opt-in through
//! [`Configuration::network`]; [`Network::Borrowed`] keeps ownership with the app.
//! [`Session::pairing`] requires an explicit [`GrantProfile`].
//!
//! Queries buffer one page; collect only results that fit in memory. Register
//! [`Database::observe_tables`] before querying to avoid missing invalidations.
//! Transactions roll back on drop and acknowledge local durability on commit.
//! They never retry application work automatically. Cancellation during commit
//! can leave its outcome uncertain; reconcile before repeating the write.
//!
//! Call [`Database::close`] at shutdown to await cleanup. See the `notes` example
//! for a complete executable application.
pub use rocky_sdk::*;
