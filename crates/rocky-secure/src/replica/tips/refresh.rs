//! Graph facts borrowed for one closed-held refresh, never SQL or reach results.
use super::*;

const MAX_QUERIES: usize = 1024;
const MAX_BYTES: usize = 1024 * 1024;

pub(in crate::replica) struct RefreshGraphContext<'a> {
    graphs: &'a BTreeMap<Scope, TipGraph>,
    target: TipId,
    active: BTreeMap<(Scope, TipId), bool>,
    structural: BTreeMap<(Scope, TipId), bool>,
    winners: BTreeMap<(Scope, TipId), Option<&'a SignedSeal>>,
    paths: BTreeMap<(Scope, TipId, TipId), FramePath>,
    entries: usize,
    bytes: usize,
    #[cfg(test)]
    hits: usize,
    #[cfg(test)]
    misses: usize,
}
impl<'a> RefreshGraphContext<'a> {
    pub(in crate::replica) fn new(graphs: &'a BTreeMap<Scope, TipGraph>, target: TipId) -> Self {
        Self {
            graphs,
            target,
            active: BTreeMap::new(),
            structural: BTreeMap::new(),
            winners: BTreeMap::new(),
            paths: BTreeMap::new(),
            entries: 0,
            bytes: 0,
            #[cfg(test)]
            hits: 0,
            #[cfg(test)]
            misses: 0,
        }
    }
    fn graph(&self, scope: Scope) -> Result<&'a TipGraph, Error> {
        self.graphs
            .get(&scope)
            .ok_or_else(|| denied("unknown tip graph"))
    }
    // Include estimated B-tree overhead as well as owned frontier capacities.
    fn retain(&mut self, bytes: usize) -> bool {
        if self.entries >= MAX_QUERIES || self.bytes.saturating_add(bytes) > MAX_BYTES {
            return false;
        }
        self.entries += 1;
        self.bytes += bytes;
        true
    }
    pub(super) fn schema_ok(
        &mut self,
        scope: Scope,
        tip: TipId,
        active: bool,
    ) -> Result<bool, Error> {
        let key = (scope, tip);
        let map = if active {
            &self.active
        } else {
            &self.structural
        };
        if let Some(ok) = map.get(&key) {
            #[cfg(test)]
            {
                self.hits += 1;
            }
            return Ok(*ok);
        }
        #[cfg(test)]
        {
            self.misses += 1;
        }
        let graph = self.graph(scope)?;
        let ok = if active {
            graph.schema(tip)
        } else {
            graph.schema_any(tip)
        }
        .is_ok();
        if self.retain(std::mem::size_of::<((Scope, TipId), bool)>() + 48) {
            let map = if active {
                &mut self.active
            } else {
                &mut self.structural
            };
            map.insert(key, ok);
        }
        Ok(ok)
    }
    pub(super) fn winner(
        &mut self,
        scope: Scope,
        tip: TipId,
    ) -> Result<Option<&'a SignedSeal>, Error> {
        let key = (scope, tip);
        if let Some(winner) = self.winners.get(&key) {
            #[cfg(test)]
            {
                self.hits += 1;
            }
            return Ok(*winner);
        }
        #[cfg(test)]
        {
            self.misses += 1;
        }
        let winner = self.graph(scope)?.seal(tip).map_err(credential)?;
        if self.retain(std::mem::size_of::<((Scope, TipId), Option<&SignedSeal>)>() + 48) {
            self.winners.insert(key, winner);
        }
        Ok(winner)
    }
    pub(super) fn visible<A: RowApplication>(
        &mut self,
        scope: Scope,
        source: TipId,
        author: u32,
        sequence: u64,
    ) -> Result<bool, Error> {
        let key = (scope, source, self.target);
        if let Some(path) = self.paths.get(&key) {
            #[cfg(test)]
            {
                self.hits += 1;
            }
            return path.admits(author, sequence);
        }
        #[cfg(test)]
        {
            self.misses += 1;
        }
        let path = Inner::<A>::frame_path(self.graph(scope)?, source, self.target)?;
        let result = path.admits(author, sequence);
        let bytes = std::mem::size_of::<((Scope, TipId, TipId), FramePath)>()
            + 48
            + path.bounds.capacity() * std::mem::size_of::<Vec<rocky_credentials::AuthorMark>>()
            + path
                .bounds
                .iter()
                .map(|bound| {
                    bound.capacity() * std::mem::size_of::<rocky_credentials::AuthorMark>()
                })
                .sum::<usize>();
        if self.retain(bytes) {
            self.paths.insert(key, path);
        }
        result
    }
    #[cfg(test)]
    pub(in crate::replica) fn stats(&self) -> (usize, usize, usize, usize) {
        (self.hits, self.misses, self.entries, self.bytes)
    }
}

#[cfg(test)]
type ReachChecks = Vec<Result<String, String>>;

#[cfg(test)]
impl<A: RowApplication> Inner<A> {
    pub(crate) fn refresh_reach_for_test(
        &self,
        inputs: &[(Scope, TipId, u32, u64)],
        fallback: bool,
    ) -> (ReachChecks, ReachChecks, (usize, usize, usize, usize)) {
        let mut context = RefreshGraphContext::new(&self.graphs, self.workspace.tip);
        if fallback {
            context.entries = MAX_QUERIES;
        }
        let mut direct = Vec::new();
        let mut shared = Vec::new();
        for &(scope, tip, author, sequence) in inputs {
            let render = |result: Result<FrameReach, Error>| {
                result
                    .map(|reach| format!("{reach:?}"))
                    .map_err(|error| error.to_string())
            };
            direct.push(render(self.frame_reach_at(scope, tip, author, sequence, 0)));
            shared.push(render(self.frame_reach_at_using(
                scope,
                tip,
                author,
                sequence,
                0,
                Some(&mut context),
            )));
        }
        (direct, shared, context.stats())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_private_malformed_cyclic_path_checks_each_bound_before_the_cycle() {
        let scope = Scope {
            mesh: rocky_core::MeshId([1; 16]),
            app: rocky_core::AppId([2; 16]),
            domain: rocky_core::DomainId([3; 16]),
        };
        let tip = TipId([4; 32]);
        let graphs = BTreeMap::new();
        let mut context = RefreshGraphContext::new(&graphs, tip);
        context.paths.insert(
            (scope, tip, tip),
            FramePath {
                bounds: vec![vec![rocky_credentials::AuthorMark {
                    author: 1,
                    sequence: 5,
                }]],
                end: PathEnd::Cyclic,
            },
        );
        assert!(
            !context
                .visible::<employees_app::Employees>(scope, tip, 1, 6)
                .unwrap()
        );
        assert!(
            matches!(context.visible::<employees_app::Employees>(scope, tip, 1, 5), Err(Error::Rejected(message)) if message == "cyclic tip graph")
        );
        assert_eq!(context.hits, 2);
        assert_eq!(context.misses, 0);
    }
}
