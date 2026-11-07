#![recursion_limit = "256"]
// The protocol suites run natively; their test support is unused in a browser.
#![cfg_attr(all(test, target_family = "wasm"), allow(dead_code, unused_imports))]
//! Rocky DB's official Rust application SDK.
//!
//! Add `rocky-sdk = "<release version>"` to your dependencies.
//! Disable default features for storage without iroh. No ORM or native binding
//! generator is needed. Run async SDK methods inside a Tokio runtime.
//!
//! ```no_run
//! use rocky_sdk::{Affinity, Column, Configuration, Failure, Schema, Table, params};
//!
//! # async fn application() -> Result<(), Failure> {
//! let schema = Schema::builder("notes", 1)
//!     .table(Table::replayed("notes")
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
//! A device answering [`Session::invitation`] waits, its `join` failing with
//! [`AwaitingAdmission`], until an issuer confirms the code both devices
//! show and admits it ([`Session::admit`]); [`Session::invitation_with`]
//! admits it on arrival with an explicit [`GrantProfile`].
//!
//! Queries buffer one page; collect only results that fit in memory. Register
//! [`Database::observe_tables`] before querying to avoid missing invalidations.
//! Transactions roll back on drop and acknowledge local durability on commit:
//! [`Database::transaction_with`] with [`Durability::Flushed`] or a later
//! [`Database::flush`] waits for power-loss durability. They never retry
//! application work automatically. [`Transaction::token`] names the
//! transaction before commit, so it can be stored with application rows;
//! after a restart or an uncertain commit, reconcile through
//! [`Database::transaction_outcome`] before repeating the write.
//!
//! # Replication
//!
//! A typed table is [`Table::replayed`] or local-only. A transaction on
//! replayed tables replicates as its intent: the statements it ran with
//! their bound values. Every replica holding a partition replays its
//! transactions in one canonical order, so replicas at one seal hold the
//! same rows.
//!
//! - **Read-modify-write.** Values computed in application code are bound
//!   as they were: `SET count = ?` after reading `count` overwrites a
//!   concurrent increment, `SET count = count + ?` keeps both.
//! - **Determinism.** Statements may use only deterministic functions;
//!   clock, random and unknown functions, `rowid` keys and schema
//!   statements are refused at commit. Bind timestamps as parameters and
//!   take keys from [`Transaction::next_id`] or bound values.
//! - **Constraints.** Unique indexes, foreign keys, [`Table::check`] and
//!   [`SchemaBuilder::assertion`] hold on every replay. A transaction whose
//!   replay fails a constraint, its author's grants or the current schema
//!   is dropped whole, in every partition.
//! - **Restrictions.** An assertion judges only the state a transaction
//!   leaves; it cannot tell that an accepted row was edited and restored.
//!   [`SchemaBuilder::restriction`] declares a transition check instead:
//!   [`LogicalRestriction::immutable`] refuses updating or deleting rows
//!   whose state before the transaction matches a predicate (`status =
//!   'posted'`), [`LogicalRestriction::append_only`] every update and
//!   delete. The predicate uses the derived-rule grammar over the row's
//!   replicated columns. Delete actions count: deleting a parent whose
//!   cascade reaches a restricted child is refused. A change that keeps
//!   every value is no update. A violation fails the commit; on replay it
//!   drops the transaction with [`DropReason::Restricted`]. Rows Rocky
//!   restores itself, from a checkpoint or a rebuild, are not checked.
//!   Corrections are new rows, such as reversals, that the application
//!   defines.
//! - **Drops and finality.** A commit stays visible until replay drops it.
//!   [`TableChange::dropped`] names dropped transactions after the commit
//!   that recorded the drop, and [`Database::transaction_outcome`] gives
//!   the [`DropReason`]. An outcome is provisional until a seal covers the
//!   transaction: [`TransactionOutcome::Applied`] or
//!   [`TransactionOutcome::Dropped`] with `sealed` set is final.
//!   [`TableChange::sealed`] names this installation's transactions a seal
//!   or checkpoint may have made final. A device that takes a seal's rows
//!   as a checkpoint learns how its own covered transactions ended from
//!   the seal's signed drop list. The list names the newest 512 drops; an
//!   older covered transaction it cannot place reads as `None`, as one past
//!   the device's 4,096 kept outcomes does.
//!   [`Configuration::create`] follows a bookmark so the owner seals
//!   routinely; an installation from `Database::create` must follow one.
//! - **Irreversible effects.** Perform an external effect (send an email,
//!   charge a card) only after [`TransactionOutcome::Applied`] with
//!   `sealed` set. Store [`Transaction::token`] in an outbox row of the
//!   same transaction; a dispatcher wakes on [`TableChange::sealed`] and
//!   after a restart reconciles every pending outbox row through
//!   [`Database::transaction_outcome`], since notifications are not
//!   replayed. A final apply is not a coordinator decision: an effect that
//!   must not happen twice across devices still needs one. Finality
//!   follows the followed seal path: if owners seal one tip concurrently,
//!   the losing seal's outcomes turn provisional again.
//!
//! # Exact values
//!
//! Exact numbers are `i64` INTEGERs and never pass through binary floating
//! point. Store an amount, quantity or rate as a scaled integer whose scale
//! the application fixes per column (cents: scale 2). The range is that of
//! `i64` in minor units: about ±9.22 × 10^16 at scale 2, ±9.22 × 10^12 at
//! scale 6. Rocky carries no decimal type and no scale; applications own
//! rounding and convert at their boundaries.
//!
//! - **Checked arithmetic.** `+`, `-`, `*` and `/` in replayed statements
//!   and assertions run as `checked_add`, `checked_sub`, `checked_mul` and
//!   `checked_div`. They follow SQLite (NULL in, NULL out; `/` truncates
//!   toward zero; division by zero is NULL; REAL operands compute in REAL)
//!   except that an INTEGER result outside `i64`, which SQLite would turn
//!   into a REAL, fails with `integer overflow`. TEXT and BLOB operands
//!   fail too. A [`Table::check`], generated column or index expression
//!   must call the functions itself. `sum` and `abs` already fail on
//!   overflow. Functions that return REAL for INTEGER input (`avg`,
//!   `total`, `round`, `mod`) are refused in replayed SQL.
//! - **Overflow.** A failure at the author is a statement or commit error;
//!   in replay it drops the transaction with [`DropReason::Constraint`].
//! - **In the database.** Sums, differences, products, truncating division
//!   and comparisons of INTEGERs are exact and can be enforced by checks
//!   and assertions, such as a header total equal to the `sum` of its lines.
//!   Rounding, allocation of remainders, currency and tax rules happen in
//!   the application, which binds the results.
//! - **Representations.** A UUID is a 16-byte BLOB in RFC 9562 byte order
//!   (`[u8; 16]`); a [`Timestamp`] is INTEGER microseconds of Unix time
//!   (UTC); JSON is TEXT as bound (`serde_json::Value` binds compact text).
//!   Rocky does not canonicalize JSON: compare documents decoded or with
//!   SQLite's `json`. JSON numbers outside `i64` are not exact.
//!
//! Call [`Database::close`] at shutdown to await cleanup. See the `notes` example
//! for a complete executable application.
mod activity;
mod backup;
mod claims;
mod configuration;
mod connection;
// The always-on peer drives its own runtime.
#[cfg(not(target_family = "wasm"))]
mod custody;
mod database;
mod delivery;
mod domain;
// The starter composition opens synchronously, which a browser cannot.
#[cfg(not(target_family = "wasm"))]
mod engine;
mod enrollment;
mod escrow;
mod generation;
mod groups;
mod identities;
mod join_requests;
mod membership;
mod mesh;
mod node;
mod owner;
mod reactor;
mod recovery;
mod registry;
mod rollout;
mod scheduling;
mod schema;
mod secrets;
mod self_enrollment;
mod service;
mod session;
// Multi-node protocol suites: native transports, threads, paused clocks and
// native temporary directories.
#[cfg(all(test, not(target_family = "wasm")))]
mod simulation;
mod sql;
mod status;
mod sync;
// Multi-node protocol suites: native transports, threads, paused clocks and
// native temporary directories.
#[cfg(all(test, not(target_family = "wasm")))]
mod test_directory;
mod tip_schemas;
mod tips;
pub mod transport;

pub use backup::{Entry as BackupEntry, Inventory as BackupInventory};
pub use claims::ClaimGrant;
pub use configuration::Configuration;
pub use connection::{
    Binding, ConnectionOptions, Durability, Page, Profile, SqlFailure, TableChange, TableEvents,
};
#[cfg(not(target_family = "wasm"))]
pub use custody::CustodyPeer;
pub use database::{Connection, Coordination, Database, NetworkService, Open, Opened, Session};
#[cfg(not(target_family = "wasm"))]
pub use engine::Engine;
pub use enrollment::{
    AwaitingAdmission, JoinRefused, NodeKeys, PublisherRequired, Supplied, route_enrollment,
};
#[cfg(feature = "qualification")]
pub use reactor::{ReactorMeasurements, ReactorSnapshot, SqlCallTiming};
/// Named domain counters whose idempotent claims hand out unique,
/// increasing values; see [`Database::claim`]. Claims go through the
/// coordination service.
#[cfg(feature = "iroh")]
pub use rocky_coordinator::counters;
/// The client of a coordination service and the bundle that pins it.
#[cfg(feature = "iroh")]
pub use rocky_coordinator::{certified::TrustBundle, service::NetworkCoordinator};
/// Coordinator refusals, such as `Unavailable` (uncertain: retry the same
/// claim) and `Expired`, inside a claim's [`Failure`].
pub use rocky_core::CoordinationError;
pub use rocky_core::replay::{DropReason, TransactionOutcome};
pub use rocky_core::{
    Affinity, Collation, ColumnDefinition, DeleteAction, ForeignKeyDefinition, IndexColumn,
    IndexDefinition, LogicalAssertion, LogicalRestriction, LogicalView, TableClass,
    TableDefinition,
};
pub use rocky_core::{NodeId, PrincipalId, Scope, SqlValue, TransactionToken};
/// Signed credentials, grant profiles and the key provider contract.
pub use rocky_credentials as credentials;
pub use rocky_credentials::Profile as GrantProfile;
/// The identity port an application supplies, and a file-backed provider.
pub use rocky_credentials::{
    FileIdentityProvider, IdentityKey, IdentityProvider, Purpose, RootKey,
};
pub use rollout::{RemovalImpact, RolloutConfirmationRequired, RolloutImpact};
pub use schema::{Column, Migration, MigrationStep, Schema, SchemaBuilder, Table};
pub use secrets::{FileSecretStore, SecretStore};
pub use service::{Network, ServiceError};
pub use session::{Admit, Discovered, Invitation, JoinRequest, LinkRequest, SeenDevice};
pub use sql::{DecodeError, FromSqlValue, IntoSqlValue, Row, Rows, Timestamp, Transaction};
pub use status::*;
pub use tips::*;
pub type Failure = Box<dyn std::error::Error + Send + Sync>;
/// Rocky's handlers for an application-owned iroh endpoint, and the iroh
/// version they attach to.
#[cfg(feature = "iroh")]
pub use rocky_iroh as iroh;

pub use self_enrollment::AwaitingLink;
