//! Execution inputs and inspection scope have different responsibilities.
//! Source-use handles are retained here, never replaced by canonical cell IDs.
use super::*;

#[derive(Debug, Clone)]
pub(crate) struct Capture {
    pub(super) scope: EnvId,
    #[cfg(feature = "compact-captures")]
    pub(super) inputs: Option<Arc<[(LocalId, ThunkId)]>>,
    #[cfg(feature = "incremental")]
    pub(super) identity: Option<Arc<identity::Signature>>,
}
impl Capture {
    pub(super) fn bindings(self, parameter: LocalId, argument: ThunkId) -> Bindings {
        #[cfg(feature = "compact-captures")]
        if let Some(inputs) = self.inputs {
            return Bindings::Captured(parameter, argument, inputs);
        }
        Bindings::One(parameter, argument)
    }
}
impl Session {
    pub(super) fn capture(&mut self, _lambda: ExprId, scope: EnvId) -> Capture {
        #[cfg(feature = "compact-captures")]
        let inputs = self.capture_inputs(_lambda, None, scope);
        Capture {
            scope,
            #[cfg(feature = "compact-captures")]
            inputs,
            #[cfg(feature = "incremental")]
            // Identity supports optional call lookup, not closure execution.
            // Build it on first use, including for imported closures.
            identity: None,
        }
    }

    #[cfg(feature = "compact-captures")]
    pub(super) fn capture_inputs(
        &mut self,
        lambda: ExprId,
        parameter: Option<LocalId>,
        scope: EnvId,
    ) -> Option<Arc<[(LocalId, ThunkId)]>> {
        let layout = self.program.expressions.capture_layouts().get(lambda)?;
        if layout.len() > 1024 {
            return None;
        }
        let mut inputs = Vec::with_capacity(layout.len());
        let mut visits = 0;
        for local in layout {
            if Some(*local) == parameter {
                continue;
            }
            let mut env = scope;
            let found = loop {
                if visits >= 4096 || self.stats.steps >= self.limits.steps {
                    return None;
                }
                visits += 1;
                self.stats.steps += 1;
                let frame = &self.envs[env];
                if let Some(t) = frame.bindings.get(local) {
                    break *t;
                }
                if frame.bindings.is_capture_boundary() {
                    return None;
                }
                env = frame.parent?;
            };
            inputs.push((*local, found));
        }
        Some(inputs.into())
    }

    #[cfg(feature = "incremental")]
    pub(super) fn call_may_match(&mut self, function: ValueId, body: ExprId) -> bool {
        if self.stats.steps >= self.limits.steps {
            return false;
        }
        // Constant read-only maintenance, like recorder charging: this cannot
        // detach heap pages, so it needs no pair of copy-work flushes.
        self.stats.steps += 1;
        self.stats.maintenance_steps += 1;
        self.memo.may_have_call(function, body)
    }

    pub(super) fn call_key(&mut self, function: ValueId, argument: CellId) -> memo::Call {
        self.cache_work(|session| session.call_key_impl(function, argument))
    }
    fn call_key_impl(&mut self, function: ValueId, argument: CellId) -> memo::Call {
        #[allow(unused_mut)]
        let mut key = memo::Call::allocated(function, argument);
        #[cfg(feature = "incremental")]
        if let Some(identity) = self.function_identity(function) {
            let work = identity.width() as u64;
            if work > self.limits.steps.saturating_sub(self.stats.steps) {
                self.stats.steps = self.limits.steps;
                return key;
            }
            self.stats.steps += work;
            key.identity = Some(identity);
        }
        #[cfg(feature = "incremental")]
        {
            key.argument_identity = self.argument_identity(argument);
            let work = key.argument_identity.as_ref().map_or(0, |i| i.width()) as u64;
            if work > self.limits.steps.saturating_sub(self.stats.steps) {
                self.stats.steps = self.limits.steps;
                key.argument_identity = None;
            } else {
                self.stats.steps += work;
            }
        }
        key
    }

    #[cfg(feature = "incremental")]
    pub(super) fn function_identity(
        &mut self,
        function: ValueId,
    ) -> Option<Arc<identity::Signature>> {
        let (parameter, body, scope, inputs) = match &self.values[function] {
            Value::Closure(parameter, body, capture) => {
                if let Some(identity) = &capture.identity {
                    // A read must not detach a shared heap page or value.
                    return Some(identity.clone());
                }
                (*parameter, *body, capture.scope, capture.inputs.clone())
            }
            _ => return None,
        };
        // Imported closures have receiving scopes but no copied identity/inputs.
        // Rebuild from current handles; never reuse a foreign cell ID.
        let inputs = inputs.or_else(|| self.capture_inputs(body, Some(parameter), scope))?;
        let identity = self.signature(parameter, body, &inputs)?;
        if self.inspection.is_none() {
            if let Value::Closure(_, _, capture) = &mut self.values[function] {
                capture.identity = Some(identity.clone());
            }
        }
        Some(identity)
    }

    #[cfg(not(feature = "incremental"))]
    pub(super) fn argument_scope_compatible(&mut self, a: CellId, b: CellId) -> bool {
        a == b
    }

    #[cfg(not(feature = "incremental"))]
    pub(super) fn call_scope_compatible(&mut self, function: ValueId, witness: ValueId) -> bool {
        function == witness
    }
}

#[cfg(feature = "incremental")]
impl Session {
    /// Conservative receiving-key slots before retaining an optional call graph.
    /// Uses prepared free-input layouts, never values or argument evaluation.
    pub(super) fn call_key_slots_bound(&self, function: ValueId, argument: CellId) -> usize {
        let function = match &self.values[function] {
            Value::Closure(parameter, body, _) => self
                .program
                .expressions
                .capture_layouts()
                .get(*body)
                .map(|locals| {
                    locals
                        .iter()
                        .filter(|local| *local != parameter)
                        .take(1025)
                        .count()
                })
                .filter(|count| *count <= 1024)
                .map_or(1, |count| 4 + count * 2),
            _ => 1,
        };
        let argument = self
            .argument_recipe(argument)
            .and_then(|(expression, _)| {
                if !self.argument_identity_eligible(expression) {
                    return None;
                }
                self.program
                    .expressions
                    .capture_layouts()
                    .get(expression)
                    .map(<[LocalId]>::len)
                    .filter(|count| *count <= 1024)
            })
            .map_or(0, |count| 3 + count * 2);
        function + argument
    }
}
