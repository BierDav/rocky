//! Coalesced repair work. The peer generation protects newer hints from completion.
use super::*;

#[derive(Clone, Debug, Default)]
pub(super) enum RepairWork {
    #[default]
    Empty,
    Scopes(std::collections::BTreeSet<ResourceScope>),
    All,
}
impl RepairWork {
    pub(super) fn pending(&self) -> bool {
        !matches!(self, Self::Empty)
    }
    pub(super) fn selected(&self) -> Option<&std::collections::BTreeSet<ResourceScope>> {
        match self {
            Self::Scopes(scopes) => Some(scopes),
            _ => None,
        }
    }
}
impl PeerRetry {
    pub(super) fn finish_repair(&mut self, generation: u64) {
        if self.work_generation == generation {
            self.pending = RepairWork::Empty;
        }
    }

    pub(super) fn repair_all(&mut self) {
        self.pending = RepairWork::All;
        self.work_generation = self.work_generation.wrapping_add(1);
    }
    pub(super) fn repair_scopes(&mut self, scopes: &[ResourceScope]) {
        if scopes.is_empty() {
            return;
        }
        if matches!(self.pending, RepairWork::Empty) {
            self.pending = RepairWork::Scopes(Default::default());
        }
        if let RepairWork::Scopes(pending) = &mut self.pending {
            pending.extend(scopes);
            if pending.len() > rocky_secure::network::link::SIGNAL_SCOPES {
                self.pending = RepairWork::All;
            }
        }
        self.work_generation = self.work_generation.wrapping_add(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn newer_hints_and_independent_full_sync_survive_an_older_completion() {
        let mut state = PeerRetry::default();
        state.repair_scopes(&[scope(1)]);
        let started = state.work_generation;
        state.repair_scopes(&[scope(2)]);
        state.finish_repair(started);
        assert_eq!(state.pending.selected().unwrap().len(), 2);
        state.repair_all();
        let full = state.work_generation;
        state.repair_scopes(&[scope(3)]);
        state.finish_repair(full);
        assert!(matches!(state.pending, RepairWork::All));
        state.finish_repair(state.work_generation);
        assert!(!state.pending.pending());
    }
    #[test]
    fn repair_hints_do_not_invalidate_the_retry_reset_generation() {
        let mut state = PeerRetry {
            generation: 7,
            ..Default::default()
        };
        state.repair_scopes(&[scope(1)]);
        let work = state.work_generation;
        state.repair_all();
        assert_eq!(state.generation, 7);
        state.finish_repair(work);
        assert!(state.pending.pending());
    }
    #[test]
    fn repair_scope_overflow_has_constant_bounded_full_work() {
        let mut state = PeerRetry::default();
        for n in 0..=rocky_secure::network::link::SIGNAL_SCOPES {
            state.repair_scopes(&[scope(n as u8)]);
        }
        assert!(matches!(state.pending, RepairWork::All));
    }
    fn scope(n: u8) -> ResourceScope {
        ResourceScope {
            domain: rocky_core::Scope {
                mesh: rocky_core::MeshId([1; 16]),
                app: rocky_core::AppId([2; 16]),
                domain: rocky_core::DomainId([3; 16]),
            },
            encryption: [n; 32],
            table: [n; 32],
        }
    }
}
