//! Explicit higher-order unravel calls. Registration checks each fixed callback
//! companion and the selected library rule; execution uses ordinary typed calls.
//! Translation returns a constraint proposal, never a validated source edit.
use crate::operation::{CallError, OperationSession, PreparedOperations, TypedValue};
use knot_checker::{
    annotation::unravel::{register_and_select_with_companions, SelectedRule, UnravelArgument},
    predicate,
    program::TypedProgram,
    ty::{Scheme, Structure},
    var::{Substitution, TypeVarId},
};
use knot_core::link::LinkedProgram;
use std::collections::{BTreeMap, BTreeSet};
use twine::Limits;

/// Position in the original forward call, not in the unravel function's prefix.
#[derive(Clone, Copy, Debug)]
pub struct CallbackBinding<'a> {
    pub argument: usize,
    pub companion: &'a str,
}
/// Names are checked module bindings, not interpreter-recognized combinator names.
/// Non-callback states retain their forward order; one state is a record and
/// multiple states are a tuple of records, as in first-order unravel contracts.
pub struct UnravelCall<'a> {
    pub forward: &'a str,
    pub rules: &'a [&'a str],
    pub callbacks: &'a [CallbackBinding<'a>],
    pub state: &'a str,
    pub request: &'a str,
}

pub struct PreparedUnravelCall {
    operations: PreparedOperations,
    selection: SelectedRule,
    rule: String,
    companions: Vec<String>,
    state: String,
    request: String,
}
impl PreparedUnravelCall {
    /// Bindings must be monomorphic specializations, as for PreparedOperations.
    /// `exports` exposes optional factories/validators/selectors to the caller;
    /// unravel execution does not assume or infer a candidate-selection policy.
    pub fn link(
        base: &LinkedProgram,
        module: &TypedProgram,
        call: UnravelCall<'_>,
        exports: &[&str],
    ) -> Result<Self, String> {
        let scheme = |name: &str| -> Result<Scheme, String> {
            let def = module
                .definition(name)
                .ok_or_else(|| format!("no unravel binding {name}"))?;
            let binding = &module.bindings[&def.binding];
            if !binding.scheme.vars.is_empty()
                || !binding.scheme.constraints.is_empty()
                || !binding.parameters.is_empty()
            {
                return Err(format!(
                    "specialize unravel binding {name} before preparing it"
                ));
            }
            Ok(binding.scheme.clone())
        };
        let mut sub = module.substitution.clone();
        let state_ty = scheme(call.state)?.ty;
        // How many value arguments the call has: the forward function's
        // arguments that are not callbacks.
        let mut forward_arity = 0usize;
        let mut rest = scheme(call.forward)?.ty;
        while let Some(Structure::Fn(_, output)) = sub.head_normal(rest) {
            forward_arity += 1;
            rest = output;
        }
        let mut positions = std::collections::BTreeSet::new();
        if call
            .callbacks
            .iter()
            .any(|c| c.argument >= forward_arity || !positions.insert(c.argument))
        {
            return Err("callback positions must be distinct valid forward-argument indexes".into());
        }
        let value_count = forward_arity - call.callbacks.len();
        // One value: the state is that value. Several: a tuple of values, or
        // a tuple of feeds for a rule that reads pins.
        let values = if value_count == 1 {
            vec![state_ty]
        } else {
            let Some(Structure::Tuple(states)) = sub.head_normal(state_ty) else {
                return Err("unravel state must be a tuple with one entry per value argument".into());
            };
            let mut as_feeds = sub.clone();
            let feeds = states
                .iter()
                .map(|s| state_input(&mut as_feeds, *s))
                .collect::<Result<Vec<_>, _>>();
            match feeds {
                Ok(values) => {
                    sub = as_feeds;
                    values
                }
                Err(_) => states,
            }
        };
        if values.is_empty() {
            return Err("unravel call needs editable value arguments".into());
        }
        let arity = values.len() + call.callbacks.len();
        let mut callback_types = BTreeMap::new();
        for callback in call.callbacks {
            if callback.argument >= arity || callback_types.contains_key(&callback.argument) {
                return Err(
                    "callback positions must be distinct valid forward-argument indexes".into(),
                );
            }
            callback_types.insert(
                callback.argument,
                (callback.companion, scheme(callback.companion)?.ty),
            );
        }
        let mut forward = scheme(call.forward)?.ty;
        let mut values = values.into_iter();
        let mut arguments = Vec::with_capacity(arity);
        for index in 0..arity {
            let Some(Structure::Fn(input, output)) = sub.head_normal(forward) else {
                return Err("forward function has too few arguments for this unravel call".into());
            };
            forward = output;
            if let Some((_, companion)) = callback_types.get(&index) {
                arguments.push(UnravelArgument::Callback {
                    forward: input,
                    companion: *companion,
                });
            } else {
                let value = values.next().expect("value argument count");
                if !predicate::same(&mut sub, input, value) {
                    return Err(format!(
                        "input state does not match forward argument {index}"
                    ));
                }
                arguments.push(UnravelArgument::Value(input));
            }
        }
        let rules = call
            .rules
            .iter()
            .map(|name| scheme(name))
            .collect::<Result<Vec<_>, _>>()?;
        let selection = register_and_select_with_companions(
            &sub,
            &module.instances_table,
            &rules,
            &arguments,
            forward,
            scheme(call.request)?.ty,
        )
        .map_err(|e| format!("unravel registration: {e:?}"))?
        .selected
        .ok_or("no compatible higher-order unravel rule")?;
        let rule = call.rules[selection.index].to_owned();
        let companions = callback_types
            .values()
            .map(|(name, _)| (*name).to_owned())
            .collect::<Vec<_>>();
        let mut names: BTreeSet<_> = exports.iter().copied().collect();
        names.extend([call.forward, rule.as_str(), call.state, call.request]);
        names.extend(companions.iter().map(String::as_str));
        let operations =
            PreparedOperations::link(base, module, &names.into_iter().collect::<Vec<_>>())?;
        Ok(Self {
            operations,
            selection,
            rule,
            companions,
            state: call.state.into(),
            request: call.request.into(),
        })
    }
    pub fn selection(&self) -> &SelectedRule {
        &self.selection
    }
    pub fn start(&self, limits: Limits) -> Result<UnravelSession, CallError> {
        let operations = self.operations.start(limits)?;
        let rule = operations.binding(&self.rule)?;
        let companions = self
            .companions
            .iter()
            .map(|n| operations.binding(n))
            .collect::<Result<_, _>>()?;
        let state = operations.binding(&self.state)?;
        let request = operations.binding(&self.request)?;
        Ok(UnravelSession {
            operations,
            rule,
            companions,
            state,
            request,
        })
    }
}

fn state_input(sub: &mut Substitution, ty: TypeVarId) -> Result<TypeVarId, String> {
    let Some(Structure::Record(mut fields, mut extension)) = sub.head_normal(ty) else {
        return Err("unravel feeds must be closed value/pinned records".into());
    };
    while let Some(row) = extension {
        let Some(Structure::Record(more, rest)) = sub.head_normal(row) else {
            return Err("unravel feeds must have closed record rows".into());
        };
        fields.extend(more);
        extension = rest;
    }
    if fields.len() != 2 || !fields.contains_key("value") || !fields.contains_key("pinned") {
        return Err("unravel feeds must have exactly value and pinned fields".into());
    }
    let value = fields["value"];
    let pinned = sub.fresh_bound(Structure::App(
        knot_canonical::ast::Ref::Builtin("Maybe".into()),
        vec![value],
    ));
    if !predicate::same(sub, pinned, fields["pinned"]) {
        return Err("unravel pinned type must match the state's value type".into());
    }
    Ok(value)
}

pub struct UnravelSession {
    operations: OperationSession,
    rule: TypedValue,
    companions: Vec<TypedValue>,
    state: TypedValue,
    request: TypedValue,
}
impl UnravelSession {
    pub fn operations(&mut self) -> &mut OperationSession {
        &mut self.operations
    }
    /// Translate the prepared request. Force/inspect the returned Maybe sIn to
    /// observe success or failure; this does not select a concrete input value.
    pub fn translate(&mut self) -> Result<TypedValue, CallError> {
        self.translate_with(
            &self.companions.clone(),
            &self.state.clone(),
            &self.request.clone(),
        )
    }
    /// Supply actual runtime companions (including returned/partially applied
    /// closures) and states with the registered types in this same session.
    /// This preserves closure identity; callbacks are not recovered from names.
    pub fn translate_with(
        &mut self,
        companions: &[TypedValue],
        state: &TypedValue,
        request: &TypedValue,
    ) -> Result<TypedValue, CallError> {
        if companions.len() != self.companions.len() {
            return Err(CallError::ArgumentCount {
                expected: self.companions.len(),
                actual: companions.len(),
            });
        }
        let mut args = companions.to_vec();
        args.extend([state.clone(), request.clone()]);
        self.operations.apply(&self.rule, &args)
    }
}
