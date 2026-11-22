//! Immutable executable IDs with a separate current-source view. Source forks
//! are opt-in until typed linkage, code rebasing and application integration land.
use super::*;
mod extension;
mod payload;
mod prepare;
mod scopes;
pub(super) use scopes::Scopes;
type OriginKey = (knot_core::NodeRef, u32, u32, bool);
fn key(origin: Origin) -> OriginKey {
    (
        origin.node,
        origin.span.start,
        origin.span.end,
        origin.role == knot_core::Role::Primary,
    )
}
#[derive(Clone)]
pub(super) struct View {
    active: std::collections::HashSet<ExprId>,
    origins: HashMap<OriginKey, Origin>,
    /// Exact receiving checked prefix and its relocation. This preserves the
    /// authority of ProgramExtension without exposing or weakening that token.
    checked: Arc<Program>,
    expressions: Vec<ExprId>,
    locals: Vec<LocalId>,
    retained_expressions: usize,
}
impl View {
    pub(super) fn accepts(&self, recipe: &evidence::Recipe, values: &Heap<Value>) -> bool {
        match recipe {
            evidence::Recipe::Thunk(_, Pending::Expr(expr, _))
            | evidence::Recipe::Historical(Some(expr)) => self.active.contains(expr),
            evidence::Recipe::Summary(code) => code
                .expression()
                .is_none_or(|expr| self.active.contains(&expr)),
            evidence::Recipe::Call(function, _) => match values.get(*function) {
                Some(Value::Closure(_, body, _)) => self.active.contains(body),
                _ => false,
            },
            _ => true,
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct SourceForkLimits {
    /// Cumulative whole-body and exact-fragment comparison allowance.
    pub code_nodes: usize,
    /// Total executable entries including old immutable versions. Rebase into a
    /// fresh checked Program through ordinary transfer when this bound is reached.
    pub retained_expressions: usize,
}
impl Default for SourceForkLimits {
    fn default() -> Self {
        Self {
            code_nodes: 200_000,
            retained_expressions: 200_000,
        }
    }
}
pub struct SourceForkAttempt {
    pub work: u64,
    pub reused_expressions: usize,
    pub appended_expressions: usize,
    pub session: Option<Session>,
}
impl Session {
    /// Share an immutable heap across checked source versions. The embedding must
    /// establish nominal/dictionary/semantic ABI compatibility first, as for
    /// `reuse_validated`. No old request handles are inherited. An incomplete
    /// preparation is a bounded miss, never a partially published source world.
    pub fn try_fork_source(
        &self,
        program: Arc<Program>,
        hosts: Arc<HostRegistry>,
        limits: Limits,
        source_limits: SourceForkLimits,
    ) -> Result<SourceForkAttempt, Outcome> {
        let mut result = SourceForkAttempt {
            work: 0,
            reused_expressions: 0,
            appended_expressions: 0,
            session: None,
        };
        if self.abandoned {
            return Err(Outcome::Abandoned);
        }
        if limits.cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        let Some(prepared) = prepare::prepare(
            &self.program,
            &program,
            &limits,
            source_limits,
            &mut result.work,
        ) else {
            return if limits.cancellation.load(Ordering::Relaxed) {
                Err(Outcome::Cancelled)
            } else {
                Ok(result)
            };
        };
        // Host signatures for newly introduced source code must also match.
        for expr in &program.expressions {
            if let Kind::Host(name, signature) = &expr.kind {
                if hosts
                    .function(name)
                    .is_none_or(|f| &f.signature != signature || f.identity.is_none())
                {
                    return Ok(result);
                }
            }
        }
        let mut fork_limits = limits.clone();
        fork_limits.steps -= result.work;
        let attempt = self.try_fork_inputs(hosts, fork_limits)?;
        result.work += attempt.work;
        let Some(mut next) = attempt.session else {
            return Ok(result);
        };
        if prepared.identity_layout && self.source_code.is_none() {
            // Separately linked copies of identical checked code need no ID or
            // source relocation. Keep host-input validation from the fork, and
            // adopt the exact receiving Arc so checked helpers append normally.
            // Historical source worlds keep their existing conservative path.
            next.program = program;
            next.limits = limits;
            next.stats.steps = result.work;
            next.stats.maintenance_steps = result.work;
            if next.stats.steps >= next.limits.steps {
                return Ok(result);
            }
            if next.limits.cancellation.load(Ordering::Relaxed) {
                return Err(Outcome::Cancelled);
            }
            result.reused_expressions = prepared.reused;
            result.session = Some(next);
            return Ok(result);
        }
        let extra = prepared
            .program
            .globals
            .iter()
            .count()
            .saturating_mul(128)
            .saturating_add(prepared.program.expressions.len()) as u64;
        if extra > limits.steps.saturating_sub(result.work) {
            return Ok(result);
        }
        result.work += extra;
        next.program = Arc::new(prepared.program);
        next.source_code = Some(Arc::new(prepared.view));
        // Stable global cells receive current recipes. Other completions retain
        // their historical code IDs and require code/read validation on demand.
        let globals = next
            .program
            .globals
            .iter()
            .map(|(g, e)| (g.clone(), *e))
            .collect::<Vec<_>>();
        next.globals
            .retain(|global, _| next.program.globals.contains_key(global));
        for (global, expression) in globals {
            if let Some(thunk) = next.globals.get(&global).copied() {
                if self.program.globals.get(&global) != Some(&expression) {
                    // Replacing this source-use slot must not mutate an aliased
                    // completion owned by another binding.
                    next.thunks[thunk.0] = Thunk {
                        origin: next.program.expressions[expression.0].origin,
                        activation: next.envs[0].activation,
                        state: ThunkState::Pending(Pending::Expr(expression, 0)),
                    };
                }
            } else {
                let thunk = next.delay_expression(expression, 0);
                next.globals.insert(global, thunk);
            }
        }
        next.global_refs = next
            .program
            .expressions
            .iter()
            .map(|expr| match &expr.kind {
                Kind::Global(global) => next.globals.get(global).copied(),
                _ => None,
            })
            .collect();
        // Visibility belongs to this receiving source world. Old verdicts cannot
        // survive an edit; the bounded lexical walker proves requested scopes and
        // memoizes them, without an eager scan of the retained heap.
        next.source_scopes = Some(Scopes::default());
        next.limits = limits;
        // Global installation's conservative charge above includes its COW page
        // writes. Drain those counters without charging the same work twice.
        next.work_clock();
        next.stats.steps = result.work;
        next.stats.maintenance_steps = result.work;
        if next.heap_entries() > next.limits.heap_entries || next.stats.steps >= next.limits.steps {
            return Ok(result);
        }
        if next.limits.cancellation.load(Ordering::Relaxed) {
            return Err(Outcome::Cancelled);
        }
        result.reused_expressions = prepared.reused;
        result.appended_expressions = prepared.appended;
        result.session = Some(next);
        Ok(result)
    }
}
impl View {
    pub(super) fn origin(&self, origin: Origin) -> Option<Origin> {
        self.origins.get(&key(origin)).copied()
    }
}

#[cfg(test)]
mod tests;
