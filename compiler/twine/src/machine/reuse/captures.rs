//! Bounded recovery of completed expression provenance and exact lexical cells.
use super::*;
pub(super) struct Observation {
    pub expr: ExprId,
    pub thunk: ThunkId,
    pub env: EnvId,
    pub completed: bool,
}
pub(super) fn observe(old: &Session, limit: usize, remaining: &mut usize) -> Vec<Observation> {
    let mut observations = Vec::new();
    for (index, thunk) in old.thunks.iter().take(limit.min(*remaining)).enumerate() {
        *remaining -= 1;
        // Source uses may have different inspection scopes. Only the owner's
        // activation describes the expression's actual execution environment.
        let (expr, completed) = match &thunk.state {
            ThunkState::Done(Ok(Completed {
                expression: Some(e),
                ..
            })) => (*e, true),
            ThunkState::Pending(Pending::Expr(e, _)) => (*e, false),
            _ => continue,
        };
        let Ok(env) = usize::try_from(thunk.activation.0) else {
            continue;
        };
        if old
            .envs
            .get(env)
            .is_none_or(|e| e.activation != thunk.activation)
        {
            continue;
        }
        observations.push(Observation {
            expr,
            thunk: ThunkId(index),
            env,
            completed,
        });
    }
    observations
}
pub(super) fn bindings(
    old: &Session,
    mut env: EnvId,
    remaining: &mut usize,
) -> Option<Vec<(LocalId, CellId)>> {
    let mut result = BTreeMap::new();
    loop {
        *remaining = remaining.checked_sub(1)?;
        let scope = old.envs.get(env)?;
        for (l, t) in scope.bindings.iter() {
            *remaining = remaining.checked_sub(1)?;
            result.entry(*l).or_insert(old.cell_id(*t));
        }
        let Some(parent) = scope.parent else { break };
        env = parent;
    }
    Some(result.into_iter().collect())
}

impl Session {
    pub(in crate::machine) fn reused_capture(
        &mut self,
        expr: ExprId,
        mut env: EnvId,
    ) -> Result<Option<CellId>, StepEnd> {
        let Some(width) = self.memo.capture_width(expr) else {
            return Ok(None);
        };
        let receiving = env;
        let mut bindings = BTreeMap::new();
        loop {
            self.tick()?;
            let scope = &self.envs[env];
            // A wider lexical frame cannot match any admitted key. Bound scratch
            // allocation by existing key width before cloning the frame's cells.
            if scope.bindings.len() > width {
                return Ok(None);
            }
            let entries = scope
                .bindings
                .iter()
                .map(|(l, t)| (*l, *t))
                .collect::<Vec<_>>();
            let parent = scope.parent;
            for (local, thunk) in entries {
                self.tick()?;
                bindings.entry(local).or_insert(self.cell_id(thunk));
                if bindings.len() > width {
                    return Ok(None);
                }
            }
            let Some(parent) = parent else { break };
            env = parent;
        }
        let key = bindings.into_iter().collect::<Vec<_>>();
        let result = self
            .memo
            .captured(expr, &key)
            .filter(|cell| self.expression_scope_valid(*cell, receiving));
        #[cfg(feature = "incremental")]
        if result.is_some() && self.limits.cache_eviction_samples > 0 && self.inspection.is_none() {
            self.memo.note_capture_hit(expr, &key);
        }
        Ok(result)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn observations_describe_owners_not_each_source_use() {
        let parsed =
            knot_syntax::parse("module Test exposing (..)\nanswer :: Int\nanswer = 1\n").unwrap();
        let canonical = knot_canonical::canonicalize_module(&parsed, None).unwrap();
        let checked = knot_checker::check::check_program(&canonical).unwrap();
        let mut s = Session::new(Arc::new(knot_core::lower(&[checked])), Limits::default());
        let owner = s.globals[&Global::Value(GlobalName {
            module: vec!["Test".into()],
            name: "answer".into(),
        })];
        let alias = s.shared_use(
            s.cell_id(owner),
            s.thunks[owner.0].origin,
            s.envs[0].activation,
        );
        let observations = observe(&s, s.thunks.len(), &mut 1000);
        assert!(observations.iter().any(|o| o.thunk == owner));
        assert!(!observations.iter().any(|o| o.thunk == alias));
    }
}
