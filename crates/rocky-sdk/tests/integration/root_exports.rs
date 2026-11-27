#[expect(
    unused_imports,
    reason = "importing each name proves the README's types are exported from the crate root"
)]
use rocky_sdk::{
    Access, Affinity, AlertKind, BlockerKind, ChangeCause, ClearsBy, Column, Configuration,
    Created, Database, DatabaseStatus, DeclaredSchema, DeleteAction, DenyReason, Device, DeviceId,
    DropReason, DroppedTransaction, Error, ErrorKind, ForeignKey, IdentityKey, IdentityProvider,
    Index, Invitation, Invite, JoinError, Limit, MigrationStep, Network, Partition, PendingLink,
    Person, PersonId, RefusedChange, Removal, Right, Role, RoleInUse, RolloutImpact, RootKey,
    RowIdentity, Schema, SchemaBuilder, SchemaError, ServiceError, SyncMode, SyncReport,
    TRANSACTION_TIMEOUT, Table, TableChange, TableEvents, Timestamp, Transaction,
    TransactionOutcome, TransactionToken, Unsupported,
};

const VARIANTS: &[&str] = &[
    "AdminDeviceLinked",
    "AdminDeviceUnlinked",
    "AdminRequired",
    "AdminStale",
    "Administer",
    "Allocate",
    "Append",
    "Authority",
    "Blob",
    "Blocked",
    "Busy",
    "Cascade",
    "Closed",
    "Custom",
    "DatabaseExists",
    "Default",
    "Denied",
    "Disabled",
    "Full",
    "IdentityRequired",
    "Internal",
    "Join",
    "JoinUnfinished",
    "Link",
    "Live",
    "Local",
    "Manual",
    "MigrationFailed",
    "NoDatabase",
    "NoPeerReached",
    "NoRoute",
    "NotGranted",
    "Off",
    "OwnerRequired",
    "PartitionNotCreated",
    "PartitionNotGranted",
    "Read",
    "Rebuild",
    "Removed",
    "Restrict",
    "SchemaMismatch",
    "SealRebootstrap",
    "Service",
    "SetNull",
    "Sign",
    "Sql",
    "Store",
    "StoreOnly",
    "StoreSuperseded",
    "Sync",
    "TimedOut",
    "TooLarge",
    "Unavailable",
    "Update",
    "Write",
    "WriteDropped",
];
const ELSEWHERE: &[&str] = &[
    "Arc",
    "Cargo",
    "Claim",
    "Claimed",
    "Clone",
    "ControlReceipt",
    "CoordinationError",
    "CounterError",
    "Counters",
    "Display",
    "Err",
    "Exhausted",
    "FromStr",
    "LocalCluster",
    "Lookup",
    "Notes",
    "Ok",
    "Options",
    "Refusal",
    "Relay",
    "OutcomeUnknown",
    "Backpressure",
    "Stream",
    "StreamExt",
    "Tenant",
    "TrustBundle",
];

fn imported() -> Vec<&'static str> {
    let source = include_str!("root_exports.rs");
    let start = source.find("use rocky_sdk::{").map_or(0, |at| at + 16);
    let end = source[start..].find("};").map_or(start, |at| start + at);
    source[start..end]
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .collect()
}

fn readme_type_names() -> std::collections::BTreeSet<String> {
    let readme = include_str!("../../../../README.md");
    readme
        .split('`')
        .skip(1)
        .step_by(2)
        .filter_map(|code| {
            let name: String = code
                .chars()
                .take_while(|character| character.is_ascii_alphanumeric() || *character == '_')
                .collect();
            let capitalized = name.starts_with(|first: char| first.is_ascii_uppercase());
            let shouted = name
                .chars()
                .all(|character| !character.is_ascii_lowercase());
            (capitalized && !shouted).then_some(name)
        })
        .collect()
}

#[test]
fn every_type_the_readme_names_is_exported_from_the_crate_root() {
    let imported = imported();
    let unexported: Vec<_> = readme_type_names()
        .into_iter()
        .filter(|name| {
            !imported.contains(&name.as_str())
                && !VARIANTS.contains(&name.as_str())
                && !ELSEWHERE.contains(&name.as_str())
        })
        .collect();
    assert!(
        unexported.is_empty(),
        "the README names {unexported:?}: import each from rocky_sdk's root here, or list it as a variant or another crate's name"
    );
}
