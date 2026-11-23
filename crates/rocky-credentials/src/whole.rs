use crate::{Error, PartitionGrant, denied};
use rocky_core::{
    ApplicationManifest, Commitment, FieldSelection, PartitionSelection, PartitionSelector,
    ResourceGrant, ResourceScope, Right, RowSelection, Scope,
};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum WholeAccess {
    Full,
    Read,
    Store,
}
impl WholeAccess {
    pub fn rights(self) -> BTreeSet<Right> {
        let rights: &[Right] = match self {
            Self::Full => &[
                Right::Read,
                Right::Insert,
                Right::Update,
                Right::Delete,
                Right::Custody,
            ],
            Self::Read => &[Right::Read, Right::Custody],
            Self::Store => &[Right::Custody],
        };
        rights.iter().copied().collect()
    }
    pub fn fields(self) -> FieldSelection {
        match self {
            Self::Store => FieldSelection::Only(BTreeSet::new()),
            Self::Full | Self::Read => FieldSelection::All,
        }
    }
    pub fn reads(self) -> bool {
        self != Self::Store
    }
}

#[derive(Clone, Debug, Default)]
pub struct WholeTables {
    tables: Arc<BTreeMap<Commitment, Covered>>,
    domain: Option<Scope>,
    digest: Commitment,
}
#[derive(Clone, Debug, Serialize)]
struct Covered {
    scope: Option<ResourceScope>,
    slots: Vec<Commitment>,
}
#[derive(Default, Deserialize)]
struct Shape {
    #[serde(default)]
    explicit_access_only: bool,
    #[serde(default)]
    partition_by: Vec<String>,
}
impl WholeTables {
    pub fn new(
        authenticated_manifest: &ApplicationManifest,
        table_scopes: &[ResourceScope],
    ) -> Result<Self, Error> {
        let fields: BTreeMap<_, _> = authenticated_manifest
            .fields
            .iter()
            .map(|field| (field.name.as_str(), field.id))
            .collect();
        let mut tables = BTreeMap::new();
        for entity in &authenticated_manifest.entities {
            let shape: Shape = serde_json::from_str(&entity.shape).unwrap_or_default();
            if shape.explicit_access_only {
                continue;
            }
            let slots = shape
                .partition_by
                .iter()
                .map(|column| {
                    fields
                        .get(format!("{}.{column}", entity.name).as_str())
                        .copied()
                        .ok_or_else(|| denied("a manifest partition column is no field"))
                })
                .collect::<Result<_, Error>>()?;
            let scope = table_scopes
                .iter()
                .find(|scope| scope.table == entity.id)
                .copied();
            tables.insert(entity.id, Covered { scope, slots });
        }
        let domain = table_scopes.first().map(|scope| scope.domain);
        let digest =
            rocky_core::commitment("rocky.whole-tables.v1", &crate::encode(&(domain, &tables))?);
        Ok(Self {
            tables: Arc::new(tables),
            domain,
            digest,
        })
    }
    pub fn digest(&self) -> Commitment {
        self.digest
    }
    pub(crate) fn covers(&self, table: Commitment) -> bool {
        self.tables.contains_key(&table)
    }
    pub fn resources(
        &self,
        whole: Option<WholeAccess>,
        explicit: &[ResourceGrant],
    ) -> Vec<ResourceGrant> {
        let mut all = explicit.to_vec();
        if let Some(whole) = whole {
            all.extend(self.tables.values().filter_map(|covered| {
                Some(ResourceGrant {
                    scope: covered.scope?,
                    rows: RowSelection::All,
                    rights: whole.rights(),
                    fields: whole.fields(),
                })
            }));
        }
        all
    }
    pub fn partitions(
        &self,
        whole: Option<WholeAccess>,
        explicit: &[PartitionGrant],
    ) -> Vec<PartitionGrant> {
        let mut all = explicit.to_vec();
        if let Some(whole) = whole {
            all.extend(
                self.tables
                    .iter()
                    .filter_map(|(table, covered)| self.whole_partition(whole, *table, covered)),
            );
        }
        all
    }
    pub fn resources_for_table(
        &self,
        whole: Option<WholeAccess>,
        explicit: &[ResourceGrant],
        table: Commitment,
    ) -> Vec<ResourceGrant> {
        self.resources(whole, explicit)
            .into_iter()
            .filter(|grant| grant.scope.table == table)
            .collect()
    }
    pub fn partitions_for(
        &self,
        whole: Option<WholeAccess>,
        explicit: &[PartitionGrant],
        table: Commitment,
    ) -> Vec<PartitionGrant> {
        self.partitions(whole, explicit)
            .into_iter()
            .filter(|grant| grant.table == table)
            .collect()
    }
    fn whole_partition(
        &self,
        whole: WholeAccess,
        table: Commitment,
        covered: &Covered,
    ) -> Option<PartitionGrant> {
        if covered.slots.is_empty() {
            return None;
        }
        Some(PartitionGrant {
            domain: covered
                .scope
                .map_or(self.domain, |scope| Some(scope.domain))?,
            table,
            selector: PartitionSelector(
                covered
                    .slots
                    .iter()
                    .map(|slot| (*slot, PartitionSelection::Any))
                    .collect(),
            ),
            rows: RowSelection::All,
            rights: whole.rights(),
            fields: whole.fields(),
        })
    }
}

#[cfg(test)]
mod tests;
