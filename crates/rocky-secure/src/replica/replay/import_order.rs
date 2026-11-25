use super::*;

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn in_dependency_chains<T>(
        &self,
        imports: Vec<T>,
        scope_of: impl Fn(&T) -> ResourceScope,
    ) -> Result<Vec<Vec<T>>, Error> {
        let scopes: Vec<ResourceScope> = imports.iter().map(&scope_of).collect();
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let mut chain_of: Vec<usize> = (0..scopes.len()).collect();
        for (at, scope) in scopes.iter().enumerate() {
            let linked = inner.linked_resource_scopes(*scope)?;
            for (other, candidate) in scopes.iter().enumerate() {
                if linked.contains(candidate) {
                    let (kept, merged) = (
                        chain_of[at].min(chain_of[other]),
                        chain_of[at].max(chain_of[other]),
                    );
                    for chain in &mut chain_of {
                        if *chain == merged {
                            *chain = kept;
                        }
                    }
                }
            }
        }
        let parents: Vec<usize> = scopes
            .iter()
            .map(|scope| inner.referenced_tables(*scope))
            .collect();
        drop(inner);
        let mut chains: Vec<Vec<(usize, T)>> = Vec::new();
        let mut first_of: BTreeMap<usize, usize> = BTreeMap::new();
        for (at, import) in imports.into_iter().enumerate() {
            let slot = *first_of.entry(chain_of[at]).or_insert_with(|| {
                chains.push(Vec::new());
                chains.len() - 1
            });
            chains[slot].push((parents[at], import));
        }
        Ok(chains
            .into_iter()
            .map(|mut chain| {
                chain.sort_by_key(|(parents, _)| *parents);
                chain.into_iter().map(|(_, import)| import).collect()
            })
            .collect())
    }
}

impl<A: RowApplication> Inner<A> {
    fn referenced_tables(&self, scope: ResourceScope) -> usize {
        let Some(typed) = self.workspace.typed.as_ref() else {
            return 0;
        };
        typed.rows.table(&scope.table).map_or(0, |table| {
            super::constraints::referenced(typed.rows.schema(), &table.sql_name).len()
        })
    }
}
