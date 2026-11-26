use super::*;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyNode<T> {
    pub items: Vec<T>,
    pub parents: Vec<usize>,
}

impl<A: RowApplication> EncryptedReplica<A> {
    pub async fn dependency_graph<T>(
        &self,
        imports: Vec<T>,
        scope_of: impl Fn(&T) -> ResourceScope,
    ) -> Result<Vec<DependencyNode<T>>, Error> {
        let count = imports.len();
        if count == 0 {
            return Ok(Vec::new());
        }
        let scopes: Vec<ResourceScope> = imports.iter().map(&scope_of).collect();
        let inner = self.inner.lock(crate::replica::Class::Background).await;
        let children = inner.scope_dependents(&scopes)?;
        drop(inner);

        let components = strongly_connected_components(&children);
        let comp_count = components.len();
        let mut comp_of = vec![0usize; count];
        for (comp, members) in components.iter().enumerate() {
            for &member in members {
                comp_of[member] = comp;
            }
        }
        let mut comp_parents = vec![BTreeSet::new(); comp_count];
        let mut comp_children = vec![BTreeSet::new(); comp_count];
        for (parent, targets) in children.iter().enumerate() {
            let from = comp_of[parent];
            for &child in targets {
                let to = comp_of[child];
                if from != to {
                    comp_parents[to].insert(from);
                    comp_children[from].insert(to);
                }
            }
        }
        let mut remaining: Vec<usize> = comp_parents.iter().map(BTreeSet::len).collect();
        let mut ready = BTreeSet::new();
        for (comp, &count) in remaining.iter().enumerate() {
            if count == 0 {
                ready.insert((components[comp][0], comp));
            }
        }
        let mut order = Vec::with_capacity(comp_count);
        let mut position_of = vec![0usize; comp_count];
        while let Some((_, comp)) = ready.pop_first() {
            position_of[comp] = order.len();
            order.push(comp);
            for &child in &comp_children[comp] {
                remaining[child] -= 1;
                if remaining[child] == 0 {
                    ready.insert((components[child][0], child));
                }
            }
        }
        let mut slots: Vec<Option<T>> = imports.into_iter().map(Some).collect();
        let mut nodes = Vec::with_capacity(comp_count);
        for comp in order {
            let mut items = Vec::with_capacity(components[comp].len());
            for &member in &components[comp] {
                if let Some(item) = slots[member].take() {
                    items.push(item);
                }
            }
            let mut parents: Vec<usize> = comp_parents[comp]
                .iter()
                .map(|&parent| position_of[parent])
                .collect();
            parents.sort_unstable();
            nodes.push(DependencyNode { items, parents });
        }
        Ok(nodes)
    }
}

fn strongly_connected_components(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    struct Tarjan<'a> {
        edges: &'a [Vec<usize>],
        next_index: usize,
        index: Vec<Option<usize>>,
        lowlink: Vec<usize>,
        on_stack: Vec<bool>,
        stack: Vec<usize>,
        components: Vec<Vec<usize>>,
    }
    impl Tarjan<'_> {
        fn visit(&mut self, node: usize) {
            let current = self.next_index;
            self.next_index += 1;
            self.index[node] = Some(current);
            self.lowlink[node] = current;
            self.stack.push(node);
            self.on_stack[node] = true;
            for &next in &self.edges[node] {
                if self.index[next].is_none() {
                    self.visit(next);
                    self.lowlink[node] = self.lowlink[node].min(self.lowlink[next]);
                } else if self.on_stack[next]
                    && let Some(idx) = self.index[next]
                {
                    self.lowlink[node] = self.lowlink[node].min(idx);
                }
            }
            if self.lowlink[node] == current {
                let mut component = Vec::new();
                while let Some(member) = self.stack.pop() {
                    self.on_stack[member] = false;
                    component.push(member);
                    if member == node {
                        break;
                    }
                }
                component.sort_unstable();
                self.components.push(component);
            }
        }
    }
    let count = edges.len();
    let mut tarjan = Tarjan {
        edges,
        next_index: 0,
        index: vec![None; count],
        lowlink: vec![0; count],
        on_stack: vec![false; count],
        stack: Vec::new(),
        components: Vec::new(),
    };
    for node in 0..count {
        if tarjan.index[node].is_none() {
            tarjan.visit(node);
        }
    }
    tarjan.components
}

impl<A: RowApplication> Inner<A> {
    fn scope_dependents(&self, scopes: &[ResourceScope]) -> Result<Vec<Vec<usize>>, Error> {
        let mut children = vec![Vec::new(); scopes.len()];
        let Some(typed) = self.workspace.typed.as_ref() else {
            return Ok(children);
        };
        let schema = typed.rows.schema();
        let has_links = schema
            .assertions
            .iter()
            .any(|assertion| assertion.tables().len() > 1)
            || schema
                .tables
                .iter()
                .any(|table| table.class == TableClass::Replayed && !table.foreign_keys.is_empty());
        if !has_links {
            return Ok(children);
        }
        let mut info = Vec::with_capacity(scopes.len());
        for scope in scopes {
            let Some(logical) = typed
                .rows
                .table(&scope.table)
                .and_then(|table| schema.table(&table.sql_name))
                .filter(|table| table.class == TableClass::Replayed)
            else {
                info.push(None);
                continue;
            };
            let values = if logical.partition_by.is_empty() {
                None
            } else {
                self.partitions
                    .get(scope)
                    .map(|tuple| super::partition::partition_values(schema, logical, tuple))
                    .transpose()?
            };
            let referenced = super::constraints::referenced(schema, &logical.sql_name);
            let mut assertion_peers = BTreeSet::new();
            for assertion in &schema.assertions {
                let tables = assertion.tables();
                if tables
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(&logical.sql_name))
                {
                    for name in tables {
                        let lower = name.to_ascii_lowercase();
                        if !lower.eq_ignore_ascii_case(&logical.sql_name) {
                            assertion_peers.insert(lower);
                        }
                    }
                }
            }
            info.push(Some((
                logical.sql_name.to_ascii_lowercase(),
                values,
                referenced,
                assertion_peers,
            )));
        }
        for (parent_idx, parent_info) in info.iter().enumerate() {
            let Some((parent_name, parent_values, parent_refs, _)) = parent_info else {
                continue;
            };
            for (child_idx, child_info) in info.iter().enumerate() {
                if parent_idx == child_idx || scopes[parent_idx].domain != scopes[child_idx].domain
                {
                    continue;
                }
                let Some((child_name, child_values, child_refs, child_assertions)) = child_info
                else {
                    continue;
                };
                if parent_values.is_some()
                    && child_values.is_some()
                    && parent_values != child_values
                {
                    continue;
                }
                if child_refs.contains(parent_name)
                    || (child_assertions.contains(parent_name) && !parent_refs.contains(child_name))
                {
                    children[parent_idx].push(child_idx);
                }
            }
        }
        Ok(children)
    }
}
