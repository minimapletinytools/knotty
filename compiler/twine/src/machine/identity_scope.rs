//! Exact same-program source-scope correspondence. Recipe identity covers only
//! execution inputs; this proof includes every lexical binding and source use.
//! It does not establish correspondence between edited programs or equal values.
use super::*;

#[derive(Clone, Copy)]
enum Pair {
    Scope(EnvId, EnvId),
    Use(ThunkId, ThunkId),
}
#[derive(Default)]
struct Correspondence {
    forward: HashMap<usize, usize>,
    backward: HashMap<usize, usize>,
}
impl Correspondence {
    /// None rejects conflicting sharing; Some(false) already has a checked or
    /// queued pair. Cycles succeed only after every queued edge has been checked.
    fn visit(&mut self, left: usize, right: usize) -> Option<bool> {
        if let Some(previous) = self.forward.get(&left) {
            return (*previous == right).then_some(false);
        }
        if self
            .backward
            .get(&right)
            .is_some_and(|previous| *previous != left)
        {
            return None;
        }
        self.forward.insert(left, right);
        self.backward.insert(right, left);
        Some(true)
    }
}
impl Session {
    pub(super) fn scopes_compatible(&mut self, a: EnvId, b: EnvId) -> bool {
        if !self.checked_source_scope(a) || !self.checked_source_scope(b) {
            return false;
        }
        if a == b {
            return true;
        }
        let mut pending = vec![Pair::Scope(a, b)];
        let mut scopes = Correspondence::default();
        let mut uses = Correspondence::default();
        let mut allowance = 4096usize;
        while let Some(pair) = pending.pop() {
            if self.limits.cancellation.load(Ordering::Relaxed) {
                return false;
            }
            let work = match pair {
                Pair::Scope(a, _) => 1 + self.envs[a].bindings.len(),
                Pair::Use(..) => 1,
            };
            if work > allowance || work as u64 > self.limits.steps.saturating_sub(self.stats.steps)
            {
                return false;
            }
            allowance -= work;
            self.stats.steps += work as u64;
            match pair {
                Pair::Scope(a, b) => {
                    match scopes.visit(a, b) {
                        None => return false,
                        Some(false) => continue,
                        Some(true) => {}
                    }
                    if a == b {
                        continue;
                    }
                    let (left, right) = (&self.envs[a], &self.envs[b]);
                    if left.origin != right.origin || left.bindings.len() != right.bindings.len() {
                        return false;
                    }
                    match (left.parent, right.parent) {
                        (Some(a), Some(b)) => pending.push(Pair::Scope(a, b)),
                        (None, None) => {}
                        _ => return false,
                    }
                    for (local, thunk) in left.bindings.iter() {
                        let Some(other) = right.bindings.lexical_get(local) else {
                            return false;
                        };
                        pending.push(Pair::Use(*thunk, *other));
                    }
                }
                Pair::Use(a, b) => {
                    match uses.visit(a.0, b.0) {
                        None => return false,
                        Some(false) => continue,
                        Some(true) => {}
                    }
                    if a == b {
                        continue;
                    }
                    let (left, right) = (&self.thunks[a.0], &self.thunks[b.0]);
                    if left.origin != right.origin {
                        return false;
                    }
                    // Activations and environments are allocated together. Check
                    // source-use scopes even when both aliases share one owner.
                    pending.push(Pair::Scope(
                        left.activation.0 as usize,
                        right.activation.0 as usize,
                    ));
                    let (left, right) = (self.cell_id(a), self.cell_id(b));
                    if left == right {
                        continue;
                    }
                    match (self.argument_recipe(left), self.argument_recipe(right)) {
                        (Some((ae, a)), Some((be, b))) if ae == be => {
                            pending.push(Pair::Scope(a, b))
                        }
                        // Introduced values, errors, and unknown recipes do not
                        // become equal merely because their output would compare equal.
                        _ => return false,
                    }
                }
            }
        }
        true
    }
}
