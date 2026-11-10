//! Schema-authenticated `granted_like` families. Signed grants keep their encoding.
use crate::{Error, PartitionGrant, denied};
use rocky_core::{
    ApplicationManifest, Commitment, FieldSelection, PartitionSelector, ResourceGrant,
    ResourceScope, RowSelection,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Default)]
pub struct GrantFamilies {
    families: std::sync::Arc<BTreeMap<Commitment, Family>>,
    digest: Commitment,
}
#[derive(Clone, Debug, Serialize)]
struct Family {
    parent: Commitment,
    table: Commitment,
    scopes: Vec<(ResourceScope, ResourceScope)>,
    slots: Option<BTreeMap<Commitment, Commitment>>,
}
#[derive(Deserialize)]
struct Declaration {
    granted_like: Option<String>,
    #[serde(default)]
    partition_by: Vec<String>,
}
impl GrantFamilies {
    /// `manifest` and the table scopes must come from the installed, authenticated
    /// schema. Partition scopes must never be supplied as table scopes.
    pub fn new(manifest: &ApplicationManifest, scopes: &[ResourceScope]) -> Result<Self, Error> {
        let tables: BTreeMap<_, _> = manifest
            .entities
            .iter()
            .map(|table| (table.name.as_str(), table))
            .collect();
        let declarations: BTreeMap<_, _> = tables
            .iter()
            .filter_map(|(name, table)| {
                serde_json::from_str::<Declaration>(&table.shape)
                    .ok()
                    .map(|declaration| (*name, declaration))
            })
            .collect();
        let fields: BTreeMap<_, _> = manifest
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.id))
            .collect();
        let mut by_table: BTreeMap<_, Vec<_>> = BTreeMap::new();
        for scope in scopes {
            by_table.entry(scope.table).or_default().push(scope);
        }
        let mut families = BTreeMap::new();
        for (name, declaration) in &declarations {
            let Some(parent) = declaration.granted_like.as_deref() else {
                continue;
            };
            let parent_declaration = declarations
                .get(parent)
                .ok_or_else(|| denied("unknown grant family"))?;
            if parent == *name || parent_declaration.granted_like.is_some() {
                return Err(denied("cyclic grant family"));
            }
            let (parent_id, table_id) = (tables[parent].id, tables[name].id);
            let slots = if declaration.partition_by.len() == parent_declaration.partition_by.len() {
                Some(
                    parent_declaration
                        .partition_by
                        .iter()
                        .zip(&declaration.partition_by)
                        .map(|(from, to)| {
                            let from = fields
                                .get(format!("{parent}.{from}").as_str())
                                .copied()
                                .ok_or_else(|| denied("unknown family partition slot"))?;
                            let to = fields
                                .get(format!("{name}.{to}").as_str())
                                .copied()
                                .ok_or_else(|| denied("unknown family partition slot"))?;
                            Ok((from, to))
                        })
                        .collect::<Result<_, Error>>()?,
                )
            } else {
                None
            };
            let pairs = by_table
                .get(&parent_id)
                .into_iter()
                .flatten()
                .flat_map(|parent| {
                    by_table
                        .get(&table_id)
                        .into_iter()
                        .flatten()
                        .filter(move |scope| scope.domain == parent.domain)
                        .map(move |table| (**parent, **table))
                })
                .collect();
            families.insert(
                table_id,
                Family {
                    parent: parent_id,
                    table: table_id,
                    scopes: pairs,
                    slots,
                },
            );
        }
        let digest = rocky_core::commitment("rocky.grant-families.v1", &crate::encode(&families)?);
        Ok(Self {
            families: std::sync::Arc::new(families),
            digest,
        })
    }

    pub(crate) fn authorizes_binding(
        &self,
        granted: &std::collections::BTreeSet<Commitment>,
        table: Commitment,
    ) -> bool {
        granted.contains(&table)
            || granted.contains(&family_binding(table))
            || self
                .families
                .get(&table)
                .is_some_and(|family| granted.contains(&family_binding(family.parent)))
    }
    pub(crate) fn authenticated(&self) -> bool {
        self.digest != [0; 32]
    }
    pub(crate) fn enabled(&self, enabled: bool) -> Self {
        if enabled {
            self.clone()
        } else {
            Self::default()
        }
    }
    pub fn for_capability(&self, capability: &crate::Capability) -> Result<Self, Error> {
        let families = capability.extended()?.families;
        if families && !self.authenticated() {
            return Err(denied("grant families require an authenticated schema"));
        }
        Ok(self.enabled(families))
    }

    pub fn is_empty(&self) -> bool {
        self.families.is_empty()
    }

    pub fn digest(&self) -> Commitment {
        self.digest
    }

    pub fn resources(&self, grants: &[ResourceGrant]) -> Vec<ResourceGrant> {
        let mut all = grants.to_vec();
        for family in self.families.values() {
            for (parent, table) in &family.scopes {
                if grants.iter().any(|grant| grant.scope == *table) {
                    continue;
                }
                all.extend(
                    grants
                        .iter()
                        .filter(|grant| grant.scope == *parent && whole(&grant.rows, &grant.fields))
                        .map(|grant| ResourceGrant {
                            scope: *table,
                            ..grant.clone()
                        }),
                );
            }
        }
        all
    }

    pub fn resources_for_table(
        &self,
        grants: &[ResourceGrant],
        table: Commitment,
    ) -> Vec<ResourceGrant> {
        let mut all: Vec<_> = grants
            .iter()
            .filter(|grant| grant.scope.table == table)
            .cloned()
            .collect();
        if let Some(family) = self.families.get(&table) {
            for (parent, scope) in &family.scopes {
                if grants.iter().any(|grant| grant.scope == *scope) {
                    continue;
                }
                all.extend(
                    grants
                        .iter()
                        .filter(|grant| grant.scope == *parent && whole(&grant.rows, &grant.fields))
                        .map(|grant| ResourceGrant {
                            scope: *scope,
                            ..grant.clone()
                        }),
                );
            }
        }
        all
    }
    pub fn partitions(&self, grants: &[PartitionGrant]) -> Vec<PartitionGrant> {
        let mut all = grants.to_vec();
        for family in self.families.values() {
            for grant in grants.iter().filter(|grant| grant.table == family.parent) {
                if grants
                    .iter()
                    .any(|own| own.domain == grant.domain && own.table == family.table)
                {
                    continue;
                }
                if let Some(grant) = family.partition(grant) {
                    all.push(grant)
                }
            }
        }
        all
    }

    /// Resolves just one table without expanding an entire wide schema.
    pub fn partitions_for(
        &self,
        grants: &[PartitionGrant],
        domain: rocky_core::Scope,
        table: Commitment,
    ) -> Vec<PartitionGrant> {
        let own: Vec<_> = grants
            .iter()
            .filter(|grant| grant.domain == domain && grant.table == table)
            .cloned()
            .collect();
        if !own.is_empty() {
            return own;
        }
        self.families.get(&table).map_or_else(Vec::new, |family| {
            grants
                .iter()
                .filter(|grant| grant.domain == domain && grant.table == family.parent)
                .filter_map(|grant| family.partition(grant))
                .collect()
        })
    }

    /// Removes a follower only when its complete grant set equals inheritance.
    /// Explicit narrower rights, rows, fields or selectors always remain.
    pub fn compact_resources(&self, grants: &[ResourceGrant]) -> Vec<ResourceGrant> {
        let mut by_scope: BTreeMap<_, Vec<&ResourceGrant>> = BTreeMap::new();
        for grant in grants {
            by_scope.entry(grant.scope).or_default().push(grant);
        }
        let mut redundant = std::collections::BTreeSet::new();
        for family in self.families.values() {
            for (parent, table) in &family.scopes {
                let (Some(parents), Some(own)) = (by_scope.get(parent), by_scope.get(table)) else {
                    continue;
                };
                let inherited: Vec<_> = parents
                    .iter()
                    .filter(|grant| whole(&grant.rows, &grant.fields))
                    .map(|grant| ResourceGrant {
                        scope: *table,
                        ..(*grant).clone()
                    })
                    .collect();
                if same(own, &inherited.iter().collect::<Vec<_>>()) {
                    redundant.insert(*table);
                }
            }
        }
        grants
            .iter()
            .filter(|grant| !redundant.contains(&grant.scope))
            .cloned()
            .collect()
    }

    pub fn compact_partitions(&self, grants: &[PartitionGrant]) -> Vec<PartitionGrant> {
        let mut by_table: BTreeMap<_, BTreeMap<_, Vec<&PartitionGrant>>> = BTreeMap::new();
        for grant in grants {
            by_table
                .entry(grant.table)
                .or_default()
                .entry(grant.domain)
                .or_default()
                .push(grant);
        }
        let mut redundant = std::collections::BTreeSet::new();
        for family in self.families.values() {
            let (Some(parents), Some(own)) =
                (by_table.get(&family.parent), by_table.get(&family.table))
            else {
                continue;
            };
            for (domain, own) in own {
                let Some(parents) = parents.get(domain) else {
                    continue;
                };
                let inherited: Vec<_> = parents
                    .iter()
                    .filter_map(|held| family.partition(held))
                    .collect();
                if same(own, &inherited.iter().collect::<Vec<_>>()) {
                    redundant.insert((*domain, family.table));
                }
            }
        }
        grants
            .iter()
            .filter(|grant| !redundant.contains(&(grant.domain, grant.table)))
            .cloned()
            .collect()
    }
}
fn family_binding(table: Commitment) -> Commitment {
    rocky_core::commitment("rocky.author-binding.grant-family.v1", &table)
}
pub(crate) fn binding_table(
    table: Commitment,
    families: bool,
    rows: &RowSelection,
    fields: &FieldSelection,
) -> Commitment {
    if families && whole(rows, fields) {
        family_binding(table)
    } else {
        table
    }
}
fn whole(rows: &RowSelection, fields: &FieldSelection) -> bool {
    *rows == RowSelection::All
        && (matches!(fields, FieldSelection::All)
            || matches!(fields, FieldSelection::Only(fields) if fields.is_empty()))
}
fn same<T: PartialEq>(left: &[T], right: &[T]) -> bool {
    !right.is_empty()
        && left.len() == right.len()
        && left.iter().all(|item| right.contains(item))
        && right.iter().all(|item| left.contains(item))
}
impl Family {
    fn partition(&self, grant: &PartitionGrant) -> Option<PartitionGrant> {
        if !whole(&grant.rows, &grant.fields) {
            return None;
        }
        let slots = self.slots.as_ref()?;
        let selector = grant
            .selector
            .0
            .iter()
            .map(|(slot, selection)| Some((*slots.get(slot)?, selection.clone())))
            .collect::<Option<BTreeMap<_, _>>>()?;
        Some(PartitionGrant {
            table: self.table,
            selector: PartitionSelector(selector),
            ..grant.clone()
        })
    }
}

#[cfg(test)]
mod tests;
