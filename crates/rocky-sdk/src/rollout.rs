use crate::{
    Failure, PersonId,
    enrollment::Settings,
    schema::{RoleName, Schema},
};
use data_encoding::HEXLOWER;
use rocky_core::{LogicalSchema, Right, RoleDefinition};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

/// A table or field a schema version removes and the rows it holds.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalImpact {
    /// The table or field.
    pub name: String,
    /// Rows that hold data in it.
    pub rows: u64,
}
/// What a schema version removes, reported before an admin device publishes it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutImpact {
    /// The database.
    pub domain: String,
    /// The installed manifest digest.
    pub installed_manifest: String,
    /// The new schema's digest.
    pub target_schema: String,
    /// Tables removed.
    pub tables: Vec<RemovalImpact>,
    /// Fields removed.
    pub fields: Vec<RemovalImpact>,
    /// Roles that lose rights.
    pub narrowed_roles: Vec<RoleName>,
    /// Pass it to `Configuration::open_confirmed` to publish.
    pub digest: String,
}
/// Opening would publish a schema that removes data or narrows a role.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutConfirmationRequired {
    /// What it removes.
    pub report: RolloutImpact,
    /// Whether a confirmation was given for a different report.
    pub mismatch: bool,
}
impl std::fmt::Display for RolloutConfirmationRequired {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(if self.mismatch {
            "removal confirmation does not match the current impact report"
        } else {
            "removal requires application confirmation of the impact report"
        })
    }
}
impl std::error::Error for RolloutConfirmationRequired {}

/// A schema version drops a role people still hold.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleInUse {
    /// The role.
    pub role: RoleName,
    /// Who holds it; change their access first.
    pub people: Vec<PersonId>,
}
impl std::fmt::Display for RoleInUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "the schema removes role {} while {} people still hold it",
            self.role,
            self.people.len()
        )
    }
}
impl std::error::Error for RoleInUse {}

type HeldRights = BTreeMap<(String, Option<String>), BTreeSet<Right>>;

fn held_rights(
    role: &RoleDefinition,
    schema: &LogicalSchema,
    target: &LogicalSchema,
) -> HeldRights {
    let mut held = HeldRights::new();
    for (table, entry) in role.tables.iter().filter_map(|entry| {
        schema
            .table(&entry.table)
            .filter(|table| target.counterpart(table).is_some())
            .map(|table| (table, entry))
    }) {
        for grant in entry.access.grants() {
            let columns: Vec<Option<String>> = match grant.columns {
                None => vec![None],
                Some(columns) => columns
                    .iter()
                    .map(|name| {
                        Some(
                            table
                                .column(name)
                                .map_or(name.as_str(), |column| column.identity())
                                .to_owned(),
                        )
                    })
                    .collect(),
            };
            for column in columns {
                held.entry((table.identity().to_owned(), column))
                    .or_default()
                    .extend(grant.rights.iter().copied());
            }
        }
    }
    held
}

fn narrows(
    (before, current): (&RoleDefinition, &LogicalSchema),
    (after, target): (&RoleDefinition, &LogicalSchema),
) -> bool {
    let newly_per_partition = after
        .per_partition
        .iter()
        .any(|group| !before.per_partition.contains(group));
    let kept = held_rights(after, target, target);
    let holds = |table: &str, column: Option<&String>, right: &Right| {
        [None, column]
            .into_iter()
            .flat_map(|column| kept.get(&(table.to_owned(), column.cloned())))
            .any(|rights| rights.contains(right))
    };
    newly_per_partition
        || held_rights(before, current, target)
            .iter()
            .any(|((table, column), rights)| {
                rights
                    .iter()
                    .any(|right| !holds(table, column.as_ref(), right))
            })
}

pub(crate) fn narrowed_roles(current: &LogicalSchema, target: &LogicalSchema) -> Vec<RoleName> {
    current
        .roles
        .iter()
        .filter(|before| {
            target
                .role(&before.name)
                .is_some_and(|after| narrows((before, current), (after, target)))
        })
        .filter_map(|role| RoleName::new(role.name.clone()).ok())
        .collect()
}

pub(crate) fn removed_roles(current: &LogicalSchema, target: &LogicalSchema) -> Vec<RoleName> {
    current
        .roles
        .iter()
        .filter(|role| target.role(&role.name).is_none())
        .filter_map(|role| RoleName::new(role.name.clone()).ok())
        .collect()
}

pub(crate) async fn confirm(
    settings: &Settings,
    path: &Path,
    schema: &Schema,
    digest: Option<&str>,
) -> Result<(), Failure> {
    if !settings.issuer() || settings.application(schema)? != crate::DeclaredSchema::Ahead {
        return Ok(());
    }
    let (current, target) = (
        settings.domain()?.logical().clone(),
        crate::domain::logical(schema, *settings.publisher.as_bytes())?,
    );
    rocky_core::classify(&current, &target).map_err(|refusal| -> Failure {
        match refusal {
            rocky_core::Unsupported::LostEarlierName { renamed, earlier } => Box::new(
                crate::Error::from(rocky_core::SchemaError::LostEarlierName { renamed, earlier }),
            ),
            refusal => {
                format!("the application schema cannot replace the installed one: {refusal}").into()
            }
        }
    })?;
    let removed = rocky_core::removed(&current, &target);
    let narrowed_roles = narrowed_roles(&current, &target);
    if removed.is_empty() && narrowed_roles.is_empty() {
        return Ok(());
    }
    let names: Vec<&str> = current
        .typed()
        .map(|table| table.sql_name.as_str())
        .collect();
    let counts =
        rocky_secure::local_row_counts(&path.with_file_name("rocky.sqlite"), &names).await?;
    let mut report = RolloutImpact {
        domain: HEXLOWER.encode(&settings.domain.0),
        installed_manifest: HEXLOWER.encode(&settings.domain()?.manifest().hash()),
        target_schema: HEXLOWER.encode(&rocky_core::commitment(
            "rocky.sdk.rollout-schema.v1",
            &serde_json::to_vec(schema)?,
        )),
        tables: removed
            .tables
            .iter()
            .map(|name| {
                Ok(RemovalImpact {
                    name: name.clone(),
                    rows: *counts.get(name).ok_or("removed table lacks its scope")?,
                })
            })
            .collect::<Result<_, Failure>>()?,
        fields: removed
            .fields
            .iter()
            .map(|name| {
                let table = name
                    .split_once('.')
                    .map(|(table, _)| table)
                    .ok_or("removed field lacks its table")?;
                Ok(RemovalImpact {
                    name: name.clone(),
                    rows: *counts
                        .get(table)
                        .ok_or("removed field lacks its table scope")?,
                })
            })
            .collect::<Result<_, Failure>>()?,
        narrowed_roles,
        digest: String::new(),
    };
    report.digest = HEXLOWER.encode(&rocky_core::commitment(
        "rocky.sdk.rollout-impact.v2",
        &serde_json::to_vec(&report)?,
    ));
    if digest == Some(report.digest.as_str()) {
        Ok(())
    } else {
        Err(RolloutConfirmationRequired {
            report,
            mismatch: digest.is_some(),
        }
        .into())
    }
}
