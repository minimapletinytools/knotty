//! Source visibility follows the complete lexical graph, including unused
//! bindings and the source-use scopes of arguments. Prove only demanded scopes;
//! execution still uses independent read validation.
use super::*;
use std::cell::RefCell;
/// Lexical graphs are immutable once evaluation can observe them. New-scope
/// proofs are revision-local and dropped when inspection recycles arena IDs.
#[derive(Clone, Default)]
pub(in crate::machine) struct Scopes {
    known: Arc<[bool]>,
    recent: RefCell<Vec<Option<bool>>>,
}
impl From<Vec<bool>> for Scopes {
    fn from(known: Vec<bool>) -> Self {
        Self {
            known: known.into(),
            recent: Default::default(),
        }
    }
}
impl Scopes {
    pub(in crate::machine) fn clone_work(&self) -> usize {
        self.recent.borrow().len()
    }
    pub(in crate::machine) fn truncate(&self, envs: usize) {
        self.recent
            .borrow_mut()
            .truncate(envs.saturating_sub(self.known.len()));
    }
}

impl Session {
    /// Retained and newly evaluated scopes can borrow old lexical values. Prove their
    /// complete source-use graph too; never expose an obsolete parent through a
    /// new child. A bounded miss hides provenance, not the value itself.
    pub(in crate::machine) fn source_scope_visibility(&self, scope: EnvId) -> (bool, u64) {
        let Some(scopes) = &self.source_scopes else {
            return (true, 0);
        };
        let recent = scopes.recent.borrow();
        let known = |id: usize| {
            scopes.known.get(id).copied().or_else(|| {
                recent
                    .get(id.checked_sub(scopes.known.len())?)
                    .copied()
                    .flatten()
            })
        };
        if let Some(visible) = known(scope) {
            return (visible, 0);
        }
        if scope >= self.envs.len() {
            return (false, 0);
        }
        let mut pending = vec![scope];
        let mut visited = std::collections::HashSet::new();
        let mut work = 0;
        let visible = (|| {
            while let Some(id) = pending.pop() {
                work += 1;
                if work > 4096 || self.limits.cancellation.load(Ordering::Relaxed) {
                    return None;
                }
                if let Some(visible) = known(id) {
                    if !visible {
                        return Some(false);
                    }
                    continue;
                }
                if !visited.insert(id) {
                    continue;
                }
                let Some(env) = self.envs.get(id) else {
                    return Some(false);
                };
                if env
                    .origin
                    .is_some_and(|o| self.current_source_origin(o).is_none())
                {
                    return Some(false);
                }
                work += env.bindings.len() as u64;
                if work > 4096 {
                    return None;
                }
                pending.extend(env.parent);
                for (_, thunk) in env.bindings.iter() {
                    let thunk = &self.thunks[thunk.0];
                    if self.current_source_origin(thunk.origin).is_none() {
                        return Some(false);
                    }
                    pending.push(thunk.activation.0 as usize);
                }
            }
            Some(true)
        })();
        drop(recent);
        if let Some(visible) = visible {
            let mut recent = scopes.recent.borrow_mut();
            let size = self.envs.len().saturating_sub(scopes.known.len());
            work += size.saturating_sub(recent.len()) as u64;
            recent.resize(size, None);
            if visible {
                for id in visited {
                    recent[id - scopes.known.len()] = Some(true);
                }
            } else {
                // Only the starting scope is proved invalid. Earlier visited
                // siblings need not depend on the invalid edge.
                recent[scope - scopes.known.len()] = Some(false);
            }
        }
        (visible.unwrap_or(false), work)
    }

    pub(in crate::machine) fn checked_source_scope(&mut self, scope: EnvId) -> bool {
        let (visible, work) = self.source_scope_visibility(scope);
        self.stats.steps = self.stats.steps.saturating_add(work);
        visible && self.stats.steps < self.limits.steps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_visibility_can_progress_after_demanding_an_ancestor() {
        let mut session = Session::new(Arc::new(Program::default()), Limits::default());
        let mut parent = 0;
        for _ in 0..5000 {
            parent = session.env(Some(parent), Bindings::Empty, None);
        }
        session.source_scopes = Some(Scopes::default());
        // An incomplete walk must not cache a negative verdict. Proving a
        // shallower scope makes this same deep scope provable on the next demand.
        assert!(!session.source_scope_visibility(parent).0);
        assert!(session.source_scope_visibility(2500).0);
        assert!(session.source_scope_visibility(parent).0);
        assert_eq!(session.source_scope_visibility(parent), (true, 0));
        // A source edit discards proofs; a snapshot of the same world keeps them.
        session.source_scopes = Some(Scopes::default());
        assert!(!session.source_scope_visibility(parent).0);
        session.limits.cancellation.store(true, Ordering::Relaxed);
        assert!(!session.source_scope_visibility(2500).0);
        session.limits.cancellation.store(false, Ordering::Relaxed);
        assert!(session.source_scope_visibility(2500).0);
    }

    #[test]
    fn cyclic_scopes_are_proved_together_and_rolled_back_ids_lose_their_verdict() {
        let mut session = Session::new(Arc::new(Program::default()), Limits::default());
        let a = session.env(Some(0), Bindings::Empty, None);
        let b = session.env(Some(a), Bindings::Empty, None);
        // Model a completed recursive lexical graph before it can be observed.
        session.envs[a].parent = Some(b);
        session.source_scopes = Some(Scopes::default());
        assert!(session.source_scope_visibility(a).0);
        assert_eq!(session.source_scope_visibility(b), (true, 0));
        session.envs.truncate(1);
        session.source_scopes.as_ref().unwrap().truncate(1);
        let reused = session.env(None, Bindings::Empty, None);
        assert_eq!(reused, a);
        // This unresolvable edge must not inherit the former scope's true bit.
        session.envs[reused].parent = Some(usize::MAX);
        assert!(!session.source_scope_visibility(reused).0);
        assert_eq!(session.source_scope_visibility(reused), (false, 0));
        assert!(session.source_scope_visibility(0).0);
    }
}
