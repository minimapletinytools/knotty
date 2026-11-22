//! Receiving-call provenance for a validated result. This stages the same generic
//! lazy graph transfer used by revision import, with exact same-program code.
use super::*;

fn bind_use(session: &Session, maps: &mut graph::Maps, old: ThunkId, new: ThunkId) -> Option<()> {
    if maps
        .thunks
        .get(&old.0)
        .is_some_and(|previous| *previous != new)
    {
        return None;
    }
    maps.thunks.insert(old.0, new);
    let (owner, receiving) = (session.cell_id(old), session.cell_id(new));
    if old != owner.owner() {
        match maps.thunks.get(&owner.0) {
            Some(previous) if session.cell_id(*previous) != receiving => return None,
            Some(_) => {}
            None => {
                maps.thunks.insert(owner.0, receiving.owner());
            }
        }
    }
    Some(())
}
impl Session {
    /// Caller has matched the exact execution key and validated its completion.
    /// A missing/ambiguous source witness is a sharing miss, never a guessed scope.
    pub(in crate::machine) fn project_call_result(
        &mut self,
        key: &memo::Call,
        first: ThunkId,
        witness: ValueId,
        argument: CellId,
        result: ValueId,
    ) -> Result<Option<ValueId>, Outcome> {
        self.tick()?;
        let mut maps = graph::Maps::default();
        let mut empty = graph::Budget {
            entries: 0,
            bytes: 0,
        };
        // The traversal's leaves contain no lexical edges and need no allocation
        // or call-body witness. This is not a separate scalar result cache.
        if graph::project(self, &maps, result, None, &mut empty).is_some() {
            return Ok(Some(result));
        }
        // An argument can itself return a closure with an unrelated lexical
        // scope. Mapping its thunk is insufficient to project a borrowed value
        // reached directly through that closure. Until those boundary values
        // have explicit correspondence, require complete argument provenance.
        if !self.argument_scope_compatible(key.argument, argument) {
            return Ok(None);
        }
        let Some(body_scope) = self.memo.body_scope(key) else {
            return Ok(None);
        };
        let (
            Value::Closure(parameter, body, receiving),
            Value::Closure(old_parameter, old_body, original),
        ) = (
            self.values[key.function].clone(),
            self.values[witness].clone(),
        )
        else {
            return Ok(None);
        };
        if parameter != old_parameter || body != old_body {
            return Ok(None);
        }
        let old_scope = &self.envs[body_scope];
        if old_scope.parent != Some(original.scope)
            || old_scope.origin != Some(self.program.expressions[body.0].origin)
        {
            return Ok(None);
        }
        let Some(old_argument) = old_scope.bindings.lexical_get(&parameter).copied() else {
            return Ok(None);
        };
        if self.cell_id(old_argument) != argument
            || bind_use(self, &mut maps, old_argument, first).is_none()
        {
            return Ok(None);
        }
        maps.envs.insert(0, 0);
        if maps
            .envs
            .insert(original.scope, receiving.scope)
            .is_some_and(|previous| previous != receiving.scope)
        {
            return Ok(None);
        }
        let Some(old_inputs) = self.capture_inputs(body, Some(parameter), original.scope) else {
            return Ok(None);
        };
        let Some(new_inputs) = self.capture_inputs(body, Some(parameter), receiving.scope) else {
            return Ok(None);
        };
        if old_inputs.len() != new_inputs.len() {
            return Ok(None);
        }
        for ((old_local, old), (new_local, new)) in old_inputs.iter().zip(new_inputs.iter()) {
            if old_local != new_local
                || !self.argument_scope_compatible(self.cell_id(*old), self.cell_id(*new))
                || bind_use(self, &mut maps, *old, *new).is_none()
            {
                return Ok(None);
            }
        }
        let scope = Env {
            empty: false,
            parent: Some(receiving.scope),
            bindings: receiving.bindings(parameter, first),
            activation: ActivationId(self.stats.activations),
            origin: Some(self.program.expressions[body.0].origin),
        };
        let allowance = self.limits.steps.saturating_sub(self.stats.steps) as usize / 2;
        let initial_entries = allowance.min(4096);
        let initial_bytes = allowance.min(1024 * 1024);
        let mut budget = graph::Budget {
            entries: initial_entries,
            bytes: initial_bytes,
        };
        let projected = graph::project(self, &maps, result, Some((body_scope, scope)), &mut budget);
        self.stats.steps +=
            (initial_entries - budget.entries + initial_bytes - budget.bytes) as u64;
        if self.limits.cancellation.load(Ordering::Relaxed) {
            self.abandoned = true;
            return Err(Outcome::Cancelled);
        }
        let Some(projected) = projected else {
            return Ok(None);
        };
        let graph::Root::Value(value) = projected.root else {
            unreachable!("value projection")
        };
        projected.commit(self, &mut maps);
        self.tick()?;
        Ok(Some(value))
    }
}
