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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemovalImpact {
    pub name: String,
    pub rows: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RolloutImpact {
    pub domain: String,
    pub installed_manifest: String,
    pub target_schema: String,
    pub tables: Vec<RemovalImpact>,
    pub fields: Vec<RemovalImpact>,
    pub narrowed_roles: Vec<RoleName>,
    pub digest: String,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RolloutConfirmationRequired {
    pub report: RolloutImpact,
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoleInUse {
    pub role: RoleName,
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

fn held_rights(role: &RoleDefinition, target: &LogicalSchema) -> HeldRights {
    let mut held = HeldRights::new();
    for entry in role
        .tables
        .iter()
        .filter(|entry| target.table(&entry.table).is_some())
    {
        for grant in entry.access.grants() {
            let columns: Vec<Option<String>> = match grant.columns {
                None => vec![None],
                Some(columns) => columns.iter().cloned().map(Some).collect(),
            };
            for column in columns {
                held.entry((entry.table.clone(), column))
                    .or_default()
                    .extend(grant.rights.iter().copied());
            }
        }
    }
    held
}

fn narrows(before: &RoleDefinition, after: &RoleDefinition, target: &LogicalSchema) -> bool {
    let newly_per_partition = after
        .per_partition
        .iter()
        .any(|group| !before.per_partition.contains(group));
    let kept = held_rights(after, target);
    let holds = |table: &str, column: Option<&String>, right: &Right| {
        [None, column]
            .into_iter()
            .flat_map(|column| kept.get(&(table.to_owned(), column.cloned())))
            .any(|rights| rights.contains(right))
    };
    newly_per_partition
        || held_rights(before, target)
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
                .is_some_and(|after| narrows(before, after, target))
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
    rocky_core::classify(&current, &target).map_err(|refusal| {
        format!("the application schema cannot replace the installed one: {refusal}")
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
