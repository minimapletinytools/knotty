//! Typed source-to-companion bridge. Library policy executes as ordinary
//! Knot; only value arguments participate in source routing and materialization.
use super::*;
use knot_checker::annotation::unravel::{register_and_select_with_companions, UnravelArgument};
use std::collections::BTreeSet;

/// Register a fixed callback position on an ordinary named forward function.
/// `expression` accepts the callback, grouped per-argument feeds, then its
/// output request, returning `Maybe` grouped per-argument requests.
/// The factory receives the actual callback closure; the engine builds the
/// `{ forward, unravel }` package. Contracts must specialize concretely at use.
#[derive(Clone, Debug)]
pub struct CallbackFactory {
    pub function: GlobalName,
    pub argument: usize,
    pub expression: String,
}
pub(super) fn validate_factories(
    factories: &[CallbackFactory],
    bodies: &[CallbackBody],
) -> Result<(), EditError> {
    let mut positions = BTreeSet::new();
    for factory in factories {
        if !positions.insert((factory.function.clone(), factory.argument)) {
            return unsupported("duplicate source callback factory position");
        }
    }
    for body in bodies {
        if !positions.insert((body.function.clone(), body.argument)) {
            return unsupported("duplicate source callback factory/body position");
        }
    }
    Ok(())
}
pub(super) struct SourceCall {
    pub name: GlobalName,
    pub args: Vec<Site>,
    pub actuals: Vec<String>,
    pub declarations: String,
    pub output: Ty,
    pub request_type: Ty,
    pub rules: usize,
    pub bodies: Vec<CallbackBody>,
}
pub(super) struct PreparedCall {
    /// The selected rule takes its value inputs plain, not as feeds.
    pub plain: bool,
    pub call: SourceCall,
    /// Callbacks the selected rule takes as plain functions, with no companion.
    pub forward_only: BTreeSet<usize>,
    pub policies: BTreeMap<usize, String>,
    pub generated: String,
    pub selected: usize,
    pub rule_type: Ty,
    pub input: Ty,
}
impl Walker<'_> {
    /// Check contracts and select the same rule for source execution and generated bodies.
    /// Ambient declarations are type witnesses only; callers bind their actual captures.
    pub(super) fn prepare_companions(
        &mut self,
        mut call: SourceCall,
        ambient: &str,
    ) -> Result<PreparedCall, EditError> {
        let mut policies: BTreeMap<_, _> = self
            .callback_factories
            .iter()
            .filter(|p| p.function == call.name)
            .map(|p| (p.argument, p.clone()))
            .collect();
        if call.rules == 0 {
            return unsupported(
                "source companion registration needs an attached higher-order rule",
            );
        }
        for (i, arg) in call.args.iter().enumerate() {
            call.declarations.push_str(&format!(
                "arg{i} :: {}\narg{i} =\n{}",
                arg.ty.as_ref().unwrap().source(),
                indent(&call.actuals[i])
            ));
        }
        let mut generated = BTreeSet::new();
        let mut generated_declarations = String::new();
        let mut contracts = Vec::new();
        let mut forward_only = BTreeSet::new();
        let inferred_selection = if policies.is_empty() && call.bodies.is_empty() {
            let (selected, inferred) = self.infer_companions(&call)?;
            forward_only = call
                .args
                .iter()
                .enumerate()
                .filter(|(i, arg)| {
                    matches!(arg.ty, Some(Ty::Function(..)))
                        && !inferred.iter().any(|(argument, _)| argument == i)
                })
                .map(|(i, _)| i)
                .collect();
            contracts = inferred;
            Some(selected)
        } else {
            None
        };
        for policy in &call.bodies {
            let i = policy.argument;
            call.args.get(i).ok_or_else(|| {
                EditError::Unsupported(
                    "source callback position is outside the forward call".into(),
                )
            })?;
            self.probes.declarations = call.declarations.clone();
            let checked = self.probes.check(&format!(
                "bodyContract = ({}) arg{i} request\n",
                policy.contract
            ))?;
            let scheme = checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel(
                    "bodyContract".into(),
                ))
                .unwrap();
            if !scheme.vars.is_empty() || !scheme.constraints.is_empty() {
                return unsupported("body companion contract must specialize concretely");
            }
            let contract = Ty::read(&mut checked.substitution.clone(), scheme.ty)?;
            contracts.push((i, contract));
        }
        for (i, contract) in contracts {
            let arg = &call.args[i];
            let Ty::Record(fields) = contract else {
                return unsupported("body contract must return a forward/unravel package");
            };
            let (inputs, _) = body::callback_shape(arg.ty.as_ref().unwrap())?;
            if fields.len() != 2 || fields.get("forward") != arg.ty.as_ref() {
                return unsupported("body contract forward type differs from callback");
            }
            let Some(Ty::Function(state, rest)) = fields.get("unravel") else {
                return unsupported("body contract needs an unravel function");
            };
            let Ty::Function(output_request, result) = &**rest else {
                return unsupported("body unravel contract needs an output request");
            };
            // One argument: its plain value. Several: plain values, or feeds.
            let (plain_state, feed_state, expected_request) = if inputs.len() == 1 {
                (
                    inputs[0].clone(),
                    None,
                    Ty::Named("Maybe".into(), vec![inputs[0].clone()]),
                )
            } else {
                (
                    Ty::Tuple(inputs.clone()),
                    Some(Ty::Tuple(inputs.iter().map(candidates::feed_type).collect())),
                    Ty::Tuple(
                        inputs
                            .iter()
                            .map(|input| Ty::Named("Maybe".into(), vec![input.clone()]))
                            .collect(),
                    ),
                )
            };
            let takes_feeds = Some(&**state) == feed_state.as_ref();
            if !(takes_feeds || **state == plain_state)
                || **result != Ty::Named("Unravelled".into(), vec![expected_request])
            {
                return unsupported(
                    "generated companions require one Maybe request per callback argument",
                );
            }
            let body = self.generate_body_in(arg, output_request, ambient)?;
            generated_declarations.push_str(&body.declarations);
            call.declarations.push_str(&body.declarations);
            // Generated code works on feeds throughout. For a companion that
            // takes plain values, wrap each value as a feed with no pin.
            let expression = if takes_feeds {
                body.factory
            } else {
                let names = (0..inputs.len())
                    .map(|n| format!("value{n}"))
                    .collect::<Vec<_>>();
                let feeds = names
                    .iter()
                    .map(|n| format!("{{ value = {n}, pinned = Nothing }}"))
                    .collect::<Vec<_>>();
                format!(
                    "(\\actual {} request -> {} actual {} request)",
                    group(&names),
                    body.factory,
                    group(&feeds)
                )
            };
            policies.insert(
                i,
                CallbackFactory {
                    function: call.name.clone(),
                    argument: i,
                    expression,
                },
            );
            generated.insert(i);
        }
        if policies.keys().any(|i| *i >= call.args.len()) {
            return unsupported("source callback position is outside the forward call");
        }
        if policies.len() + forward_only.len() == call.args.len() {
            return unsupported("source companion calls need at least one ordinary value argument");
        }
        for (i, policy) in &policies {
            call.declarations.push_str(&format!("unravelFactory{i} =\n{}companion{i} = {{ forward = arg{i}, unravel = unravelFactory{i} arg{i} }}\n", indent(&policy.expression)));
        }
        self.probes.declarations.clear();
        let checked = self.probes.check(&call.declarations)?;
        let scheme = |name: &str| {
            checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel(name.into()))
                .unwrap()
                .clone()
        };
        let arguments = (0..call.args.len())
            .map(|i| {
                let forward = scheme(&format!("arg{i}")).ty;
                if policies.contains_key(&i) {
                    UnravelArgument::Callback {
                        forward,
                        companion: scheme(&format!("companion{i}")).ty,
                    }
                } else if forward_only.contains(&i) {
                    UnravelArgument::InferredCallback(forward)
                } else {
                    UnravelArgument::Value(forward)
                }
            })
            .collect::<Vec<_>>();
        // The checked frame retained the exact forward specialization. Using
        // the frame's output avoids constructing a type arena by hand.
        let mut sub = checked.substitution.clone();
        let Some(knot_checker::ty::Structure::Tuple(frame)) = sub.head_normal(scheme("frame").ty)
        else {
            return Err(EditError::Internal("higher-order frame type".into()));
        };
        if Ty::read(&mut sub, frame[0])? != call.output {
            return Err(EditError::Internal(
                "higher-order output specialization changed".into(),
            ));
        }
        let rule_indices =
            inferred_selection.map_or_else(|| (0..call.rules).collect::<Vec<_>>(), |i| vec![i]);
        let rules = rule_indices
            .iter()
            .map(|i| scheme(&format!("rule{i}")))
            .collect::<Vec<_>>();
        let selected = register_and_select_with_companions(
            &checked.substitution,
            &checked.instances_table,
            &rules,
            &arguments,
            frame[0],
            scheme("request").ty,
        )
        .map_err(|e| EditError::Check(format!("{e:?}")))?
        .selected
        .ok_or_else(|| {
            EditError::Unsupported("no compatible higher-order unravel annotation".into())
        })?;
        let plain = selected.plain;
        let mut sub = selected.substitution;
        let input = Ty::read(&mut sub, selected.input_constraint)?;
        let companions = selected
            .callbacks
            .iter()
            .map(|c| Ok((c.argument, Ty::read(&mut sub, c.companion)?)))
            .collect::<Result<BTreeMap<_, _>, EditError>>()?;
        let mut rule_type = candidates::written_rule_type(
            call.args
                .iter()
                .enumerate()
                .filter(|(i, _)| !policies.contains_key(i) && !forward_only.contains(i))
                .map(|(_, arg)| arg.ty.clone().unwrap()),
            &call.request_type,
            input.clone(),
            plain,
        );
        // Leading parameters, in argument order: a companion, or the callback itself.
        for i in (0..call.args.len()).rev() {
            let leading = if let Some(companion) = companions.get(&i) {
                companion.clone()
            } else if forward_only.contains(&i) {
                call.args[i].ty.clone().unwrap()
            } else {
                continue;
            };
            rule_type = Ty::Function(Box::new(leading), Box::new(rule_type));
        }
        for i in &forward_only {
            self.pin_source_dependencies(call.args[*i].clone())?;
        }
        // Freeze source dependencies, including transitive lexical/global
        // captures and factory code. This needs neither Eq nor decoding of a
        // function. It is deliberately stricter than value-based capture pins.
        for i in policies.keys() {
            self.pin_source_dependencies(call.args[*i].clone())?;
            if generated.contains(i) {
                // Compiler pins every selected rule/selector separately; its
                // generated helper names are not source dependencies.
                continue;
            }
            let factory = checked
                .canonical
                .decls
                .iter()
                .find_map(|decl| match &decl.node {
                    CDecl::Fn(f) if f.name == format!("unravelFactory{i}") => Some(f),
                    _ => None,
                })
                .ok_or_else(|| EditError::Internal("missing checked unravel factory".into()))?;
            self.pin_source_dependencies(Site {
                recipe: None,
                candidate_source: false,
                via: None,
                lash: None,
                cleat: None,
                synthetic: false,
                expr: factory.body.clone(),
                module: checked.canonical.name.clone(),
                scope: 0,
                projections: vec![],
                locked: true,
                fixed: true,
                ty: Some(Ty::Function(
                    Box::new(call.args[*i].ty.clone().unwrap()),
                    Box::new(match &companions[i] {
                        Ty::Record(fields) => fields["unravel"].clone(),
                        _ => return Err(EditError::Internal("companion dependency type".into())),
                    }),
                )),
            })?;
        }
        Ok(PreparedCall {
            plain,
            call,
            forward_only,
            policies: policies
                .into_iter()
                .map(|(i, p)| (i, p.expression))
                .collect(),
            generated: generated_declarations,
            selected: rule_indices[selected.index],
            rule_type,
            input,
        })
    }
    /// Select on types before generating any callback body. Each function
    /// argument receives the corresponding companion in forward argument order.
    fn infer_companions(
        &mut self,
        call: &SourceCall,
    ) -> Result<(usize, Vec<(usize, Ty)>), EditError> {
        self.probes.declarations.clear();
        let checked = self.probes.check(&call.declarations)?;
        let scheme = |name: &str| {
            checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel(name.into()))
                .unwrap()
                .clone()
        };
        let arguments = call
            .args
            .iter()
            .enumerate()
            .map(|(i, arg)| {
                let ty = scheme(&format!("arg{i}")).ty;
                if matches!(arg.ty, Some(Ty::Function(..))) {
                    UnravelArgument::InferredCallback(ty)
                } else {
                    UnravelArgument::Value(ty)
                }
            })
            .collect::<Vec<_>>();
        let mut sub = checked.substitution.clone();
        let Some(knot_checker::ty::Structure::Tuple(frame)) = sub.head_normal(scheme("frame").ty)
        else {
            return Err(EditError::Internal("higher-order frame type".into()));
        };
        let rules = (0..call.rules)
            .map(|i| scheme(&format!("rule{i}")))
            .collect::<Vec<_>>();
        let selected = register_and_select_with_companions(
            &sub,
            &checked.instances_table,
            &rules,
            &arguments,
            frame[0],
            scheme("request").ty,
        )
        .map_err(|e| EditError::Check(format!("{e:?}")))?
        .selected
        .ok_or_else(|| {
            EditError::Unsupported("no compatible higher-order unravel annotation".into())
        })?;
        let mut sub = selected.substitution;
        let contracts = selected
            .callbacks
            .iter()
            .map(|callback| Ok((callback.argument, Ty::read(&mut sub, callback.companion)?)))
            .collect::<Result<Vec<_>, EditError>>()?;
        Ok((selected.index, contracts))
    }
    /// `site` is the higher-order call itself: a `Splice` replaces all of it.
    pub(super) fn higher_order_call(
        &mut self,
        call: SourceCall,
        site: &Site,
        incoming: &TypedValue,
    ) -> Result<(), EditError> {
        let request_type = call.request_type.clone();
        let PreparedCall {
            mut call,
            policies,
            forward_only,
            selected,
            input,
            plain,
            ..
        } = self.prepare_companions(call, "")?;
        let leading = (0..call.args.len())
            .filter_map(|i| {
                if policies.contains_key(&i) {
                    Some(format!("companion{i}"))
                } else if forward_only.contains(&i) {
                    Some(format!("arg{i}"))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        let values = call
            .args
            .into_iter()
            .enumerate()
            .filter(|(i, _)| !policies.contains_key(i) && !forward_only.contains(i))
            .collect::<Vec<_>>();
        for (_, arg) in &values {
            self.reject_partial_solver_pin(arg)?;
        }
        let pins =
            self.retain_argument_pins(&values.iter().map(|(_, a)| a.clone()).collect::<Vec<_>>())?;
        let mut states = Vec::new();
        for ((i, arg), pinned) in values.iter().zip(&pins) {
            let pin = if *pinned {
                format!("Just {}", self.pin_source(arg, &format!("arg{i}"))?)
            } else {
                "Nothing".into()
            };
            states.push(format!("{{ value = arg{i}, pinned = {pin} }}"));
        }
        if plain {
            // The rule takes values. Pins are still retained above and
            // checked at the end; the rule just does not see them.
            states = values.iter().map(|(i, _)| format!("arg{i}")).collect();
        }
        let state_types = values
            .iter()
            .map(|(_, arg)| {
                if plain {
                    arg.ty.clone().unwrap()
                } else {
                    candidates::feed_type(arg.ty.as_ref().unwrap())
                }
            })
            .collect::<Vec<_>>();
        let state_type = if state_types.len() == 1 {
            state_types[0].clone()
        } else {
            Ty::Tuple(state_types)
        };
        call.declarations.push_str(&format!(
            "states :: {}\nstates = {}\n",
            state_type.source(),
            group(&states)
        ));
        // The original checked declarations already include `request`.
        self.probes.declarations = call.declarations;
        let (proposal, _) = self.probes.evaluate_lazy_call(
            &format!("rule{} {}", selected, leading.join(" ")),
            &[("states", state_type), ("request", call.request_type)],
            &Ty::Named("Unravelled".into(), vec![input.clone()]),
        )?;
        let request = match self.probes.unravelled_payload(&proposal, &input)? {
            RuleAnswer::Unravel(value) => value,
            RuleAnswer::Splice => return self.materialize(site, incoming, &request_type),
            RuleAnswer::Snag => {
                self.note_snag(site, incoming);
                return Err(EditError::RuleFailed);
            }
        };
        if values.len() == 1 {
            return self.schedule(values.into_iter().next().unwrap().1, request, input);
        }
        let Ty::Tuple(types) = input else {
            return Err(EditError::Internal(
                "higher-order input request tuple".into(),
            ));
        };
        if types.len() != values.len() {
            return Err(EditError::Internal(
                "higher-order input request arity".into(),
            ));
        }
        for (i, ((_, site), ty)) in values.into_iter().zip(types).enumerate() {
            let request = self.probes.project_live(&request, Field::Index(i))?;
            self.schedule(site, request, ty)?;
        }
        Ok(())
    }
}
