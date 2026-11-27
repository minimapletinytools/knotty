//! Ordered unravel-rule registration at a known forward specialization.
//! Each rule owns an isolated type arena: selection never refines caller types
//! or another candidate. Evidence is retained in that arena for typed companions.
use crate::{
    interface::instance::{resolve, Evidence, InstanceTable},
    predicate,
    print::print_type,
    solve::{free_vars, instantiate},
    ty::{Scheme, Structure},
    unify::{structure_children, unify},
    var::{Substitution, TypeVarId},
};
use knot_canonical::ast::Ref;
use std::collections::{BTreeMap, HashMap, HashSet};

/// Callback positions are supplied by the caller. Their functions are fixed capabilities, not
/// editable values requiring an Eq/function-valued Constriction dictionary.
#[derive(Clone, Copy, Debug)]
pub enum UnravelArgument {
    Value(TypeVarId),
    /// Infer the package from the corresponding leading rule parameter.
    InferredCallback(TypeVarId),
    Callback {
        forward: TypeVarId,
        companion: TypeVarId,
    },
}

/// Type IDs belong to the owning registered/selected rule's substitution.
#[derive(Clone, Debug)]
pub struct CallbackContract {
    pub argument: usize,
    /// Invocation arity from the companion's feed shape, checked against forward.
    pub arity: usize,
    pub companion: TypeVarId,
    pub input_constraint: TypeVarId,
    pub output_constraint: TypeVarId,
    pub input_validation: Evidence,
    pub output_validation: Evidence,
}

#[derive(Debug)]
pub struct RegisteredRule {
    pub substitution: Substitution,
    pub rule_type: TypeVarId,
    pub input_constraint: TypeVarId,
    pub output_constraint: TypeVarId,
    pub input_validation: Evidence,
    pub output_validation: Evidence,
    pub callbacks: Vec<CallbackContract>,
    obligations: Vec<(TypeVarId, String)>,
}
#[derive(Debug)]
pub struct SelectedRule {
    /// Index in attachment order, independent of helper-definition order.
    pub index: usize,
    pub exact: bool,
    pub substitution: Substitution,
    pub input_constraint: TypeVarId,
    pub output_constraint: TypeVarId,
    pub rule_dictionaries: Vec<Evidence>,
    pub input_validation: Evidence,
    pub output_validation: Evidence,
    pub callbacks: Vec<CallbackContract>,
}
#[derive(Debug)]
pub struct RuleSelection {
    pub registered: Vec<RegisteredRule>,
    pub selected: Option<SelectedRule>,
}
#[derive(Debug)]
pub struct RegistrationError {
    pub rule: Option<usize>,
    pub message: String,
}

/// Whether a rule's first parameter is for a callback: a companion record, or
/// the callback itself as a plain function.
pub fn has_companion_prefix(sub: &Substitution, scheme: &Scheme) -> bool {
    let mut sub = sub.clone();
    let Some(Structure::Fn(parameter, _)) = sub.head_normal(scheme.ty) else {
        return false;
    };
    let (fields, extension) = match sub.head_normal(parameter) {
        Some(Structure::Record(fields, extension)) => (fields, extension),
        // A callback taken as the plain function. Feeds are never functions.
        Some(Structure::Fn(..)) => return true,
        _ => return false,
    };
    let (fields, _) = crate::interface::instance::flatten_record(&mut sub, fields, extension);
    fields.contains_key("forward") && fields.contains_key("unravel")
}

/// All type IDs and schemes must belong to `sub`. Forward input/output and
/// incoming request types must already be closed; signature inference stays
/// with the caller. Every rule is checked even if an earlier one wins.
/// A valid rule with unsatisfied specialization constraints is inapplicable;
/// malformed contracts are registration errors. Conversion search is separate.
pub fn register_and_select_unary(
    sub: &Substitution,
    table: &InstanceTable,
    rules: &[Scheme],
    input: TypeVarId,
    output: TypeVarId,
    request: TypeVarId,
) -> Result<RuleSelection, RegistrationError> {
    register_and_select(sub, table, rules, &[input], output, request)
}

/// Arguments are explicit: a unary tuple argument is not a multiargument call.
/// Two or more arguments use a tuple of states and a tuple input validator.
pub fn register_and_select(
    sub: &Substitution,
    table: &InstanceTable,
    rules: &[Scheme],
    inputs: &[TypeVarId],
    output: TypeVarId,
    request: TypeVarId,
) -> Result<RuleSelection, RegistrationError> {
    register_and_select_with_companions(
        sub,
        table,
        rules,
        &inputs
            .iter()
            .copied()
            .map(UnravelArgument::Value)
            .collect::<Vec<_>>(),
        output,
        request,
    )
}

/// Higher-order rules receive one leading `{ forward, unravel }` companion per
/// callback, in forward-argument order, then the ordinary grouped value states
/// and output request. Explicit packages must be concrete; inferred packages
/// specialize independently for each rule from forward types and the request.
/// This registers evidence; it neither chooses callback inverses nor executes
/// a rule. Prefixes use ordinary function application, with no function names.
pub fn register_and_select_with_companions(
    sub: &Substitution,
    table: &InstanceTable,
    rules: &[Scheme],
    arguments: &[UnravelArgument],
    output: TypeVarId,
    request: TypeVarId,
) -> Result<RuleSelection, RegistrationError> {
    let mut base = sub.clone();
    let mut parameters = Vec::new();
    let mut inputs = Vec::new();
    for (index, arg) in arguments.iter().enumerate() {
        match arg {
            UnravelArgument::Value(ty) => inputs.push(*ty),
            UnravelArgument::InferredCallback(forward) => {
                parameters.push((index, *forward, None));
            }
            UnravelArgument::Callback { forward, companion } => {
                check_companion(&mut base, table, index, *forward, *companion, false).map_err(
                    |message| RegistrationError {
                        rule: None,
                        message,
                    },
                )?;
                parameters.push((index, *forward, Some(*companion)));
            }
        }
    }
    if inputs.is_empty() {
        return Err(RegistrationError {
            rule: None,
            message: "an unravel call requires at least one editable value argument".into(),
        });
    }
    let states: Vec<_> = inputs
        .iter()
        .map(|&input| {
            let pinned =
                base.fresh_bound(Structure::App(Ref::Builtin("Maybe".into()), vec![input]));
            base.fresh_bound(Structure::Record(
                BTreeMap::from([("value".into(), input), ("pinned".into(), pinned)]),
                None,
            ))
        })
        .collect();
    let group = |sub: &mut Substitution, types: &[TypeVarId]| {
        if types.len() == 1 {
            types[0]
        } else {
            sub.fresh_bound(Structure::Tuple(types.to_vec()))
        }
    };
    let input = group(&mut base, &inputs);
    let expected_state = group(&mut base, &states);
    if ![input, output, request]
        .into_iter()
        .all(|t| closed(&mut base, t))
    {
        return Err(RegistrationError {
            rule: None,
            message: "forward and request types must be known before unravel dispatch".into(),
        });
    }
    let mut registered = Vec::with_capacity(rules.len());
    let mut selected: Option<SelectedRule> = None;
    for (index, scheme) in rules.iter().enumerate() {
        let contract = register(
            &base,
            table,
            scheme,
            input,
            output,
            expected_state,
            &parameters,
        )
        .map_err(|message| RegistrationError {
            rule: Some(index),
            message,
        })?;
        let mut matched = contract.substitution.clone();
        let exact = predicate::same(&mut matched, contract.output_constraint, request);
        // The request is closed, and only fresh rule variables can be solved.
        if unify(&mut matched, contract.output_constraint, request).is_ok() {
            let dictionaries = contract
                .obligations
                .iter()
                .map(|(ty, interface)| {
                    complete(&mut matched, table, &HashMap::new(), interface, *ty)
                })
                .collect::<Result<Vec<_>, _>>();
            if let Ok(rule_dictionaries) = dictionaries {
                let evidence = validators(
                    &mut matched,
                    table,
                    &HashMap::new(),
                    input,
                    output,
                    contract.input_constraint,
                    contract.output_constraint,
                );
                let callbacks = contract
                    .callbacks
                    .iter()
                    .map(|callback| {
                        let forward = parameters
                            .iter()
                            .find(|(index, _, _)| *index == callback.argument)
                            .map(|(_, forward, _)| *forward)
                            .expect("a callback contract has a callback argument");
                        check_companion(
                            &mut matched,
                            table,
                            callback.argument,
                            forward,
                            callback.companion,
                            false,
                        )
                    })
                    .collect::<Result<Vec<_>, _>>();
                if let (Ok((input_validation, output_validation)), Ok(callbacks)) =
                    (evidence, callbacks)
                {
                    if selected.is_none() {
                        selected = Some(SelectedRule {
                            index,
                            exact,
                            substitution: matched,
                            input_constraint: contract.input_constraint,
                            output_constraint: contract.output_constraint,
                            rule_dictionaries,
                            input_validation,
                            output_validation,
                            callbacks,
                        });
                    }
                }
            }
        }
        registered.push(contract);
    }
    Ok(RuleSelection {
        registered,
        selected,
    })
}

fn register(
    sub: &Substitution,
    table: &InstanceTable,
    scheme: &Scheme,
    input: TypeVarId,
    output: TypeVarId,
    expected_state: TypeVarId,
    parameters: &[(usize, TypeVarId, Option<TypeVarId>)],
) -> Result<RegisteredRule, String> {
    let mut sub = sub.clone();
    let (rule_type, obligations) = instantiate(&mut sub, scheme);
    let mut body = rule_type;
    let mut callbacks = Vec::new();
    for &(argument, forward, companion) in parameters {
        let Some(Structure::Fn(parameter, rest)) = sub.head_normal(body) else {
            return Err("unravel rule must accept its callback companions first".into());
        };
        // A rule may take a callback as the plain function. It then gets the
        // forward function only, and the callback needs no reverse rule.
        if companion.is_none() && matches!(sub.head_normal(parameter), Some(Structure::Fn(..))) {
            unify(&mut sub, parameter, forward)
                .map_err(|_| format!("callback type mismatch for argument {argument}"))?;
            body = rest;
            continue;
        }
        if let Some(companion) = companion {
            unify(&mut sub, parameter, companion)
                .map_err(|_| format!("incompatible unravel companion for argument {argument}"))?;
        }
        let Some(Structure::Record(fields, extension)) = sub.head_normal(parameter) else {
            return Err("unravel rule must accept a companion for each inferred callback; use explicit callback registration to disambiguate positions".into());
        };
        let (fields, _) = crate::interface::instance::flatten_record(&mut sub, fields, extension);
        let Some(package_forward) = fields.get("forward") else {
            return Err("unravel rule is missing a callback companion; use explicit callback registration to disambiguate positions".into());
        };
        unify(&mut sub, *package_forward, forward)
            .map_err(|_| format!("incompatible unravel companion for argument {argument}"))?;
        callbacks.push((argument, forward, parameter));
        body = rest;
    }
    let Some(Structure::Fn(state, rest)) = sub.head_normal(body) else {
        return Err("unravel rule must accept a Feed state and an output constraint".into());
    };
    let Some(Structure::Fn(output_constraint, result)) = sub.head_normal(rest) else {
        return Err("unravel rule must accept an output constraint as its second argument".into());
    };
    let Some(Structure::App(head, args)) = sub.head_normal(result) else {
        return Err("unravel rule must return Unravelled sIn".into());
    };
    if head != Ref::Builtin("Unravelled".into()) || args.len() != 1 {
        return Err("unravel rule must return Unravelled sIn".into());
    }
    let input_constraint = args[0];
    unify(&mut sub, state, expected_state).map_err(|_| {
        "rule input must accept one { value : a, pinned : Maybe a } state per forward argument"
            .to_string()
    })?;
    // After fixing forward types, request types must determine the remaining
    // polymorphism. Never choose an upstream sensitivity by instance search.
    let request_vars: HashSet<_> = free_vars(&mut sub, output_constraint).into_iter().collect();
    for (argument, forward, companion) in callbacks.iter() {
        let explicit = parameters
            .iter()
            .find(|(index, _, _)| index == argument)
            .and_then(|(_, _, explicit)| *explicit);
        if explicit.is_none() {
            if let Some(Structure::Record(fields, _)) = sub.head_normal(*companion) {
                default_callback_requests(
                    &mut sub,
                    *forward,
                    fields.get("unravel").copied(),
                    &request_vars,
                )?;
            }
        }
    }
    if std::iter::once(rule_type)
        .chain(obligations.iter().map(|(ty, _)| *ty))
        .flat_map(|t| free_vars(&mut sub, t))
        .any(|v| !request_vars.contains(&v))
    {
        return Err(
            "rule has type variables not determined by the forward input and incoming request"
                .into(),
        );
    }
    let mut given: HashMap<_, HashSet<_>> = HashMap::new();
    for (ty, interface) in &obligations {
        if closed(&mut sub, *ty) {
            complete(&mut sub, table, &HashMap::new(), interface, *ty)?;
        } else {
            given
                .entry(sub.find(*ty))
                .or_default()
                .insert(interface.clone());
        }
    }
    // Validate only after forward/state specialization and the determinacy
    // check. Generic request evidence remains conditional until selection.
    let callbacks = callbacks
        .into_iter()
        .map(|(argument, forward, companion)| {
            check_companion(&mut sub, table, argument, forward, companion, true)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // Validation is a protocol requirement, even when the rule's own body
    // does not call fits. Generic requirements remain conditional until
    // the incoming request fixes their types; concrete missing instances fail.
    let in_pair = predicate::pack(&mut sub, vec![input_constraint, input]);
    let out_pair = predicate::pack(&mut sub, vec![output_constraint, output]);
    let input_validation = resolve(&mut sub, table, &given, "Constriction", in_pair)
        .map_err(|e| format!("input validator: {e:?}"))?;
    let output_validation = resolve(&mut sub, table, &given, "Constriction", out_pair)
        .map_err(|e| format!("output validator: {e:?}"))?;
    Ok(RegisteredRule {
        substitution: sub,
        rule_type,
        input_constraint,
        output_constraint,
        input_validation,
        output_validation,
        callbacks,
        obligations,
    })
}

// Only unconstrained input-request variables of inferred companions receive the
// generated exact default. Explicit companions and declared request shapes win.
fn default_callback_requests(
    sub: &mut Substitution,
    mut forward: TypeVarId,
    unravel: Option<TypeVarId>,
    determined: &HashSet<TypeVarId>,
) -> Result<(), String> {
    let Some(Structure::Fn(_, rest)) = unravel.and_then(|ty| sub.head_normal(ty)) else {
        return Ok(());
    };
    let Some(Structure::Fn(_, result)) = sub.head_normal(rest) else {
        return Ok(());
    };
    let Some(Structure::App(head, requests)) = sub.head_normal(result) else {
        return Ok(());
    };
    if head != Ref::Builtin("Unravelled".into()) || requests.len() != 1 {
        return Ok(());
    }
    let mut inputs = Vec::new();
    while let Some(Structure::Fn(input, rest)) = sub.head_normal(forward) {
        inputs.push(input);
        forward = rest;
    }
    let exact = inputs
        .iter()
        .map(|&input| sub.fresh_bound(Structure::App(Ref::Builtin("Maybe".into()), vec![input])))
        .collect::<Vec<_>>();
    let request = requests[0];
    if sub.head_normal(request).is_none()
        && !determined.contains(&sub.find(request))
        && !exact.is_empty()
    {
        let default = if exact.len() == 1 {
            exact[0]
        } else {
            sub.fresh_bound(Structure::Tuple(exact))
        };
        unify(sub, request, default)
            .map_err(|_| "incompatible generated callback request".to_string())?;
    } else if let Some(Structure::Tuple(parts)) = sub.head_normal(request) {
        if parts.len() == exact.len() {
            for (part, default) in parts.into_iter().zip(exact) {
                if sub.head_normal(part).is_none() && !determined.contains(&sub.find(part)) {
                    unify(sub, part, default)
                        .map_err(|_| "incompatible generated callback request".to_string())?;
                }
            }
        }
    }
    Ok(())
}

fn check_companion(
    sub: &mut Substitution,
    table: &InstanceTable,
    argument: usize,
    forward: TypeVarId,
    companion: TypeVarId,
    generic: bool,
) -> Result<CallbackContract, String> {
    if !closed(sub, forward) || (!generic && !closed(sub, companion)) {
        return Err("callback and companion types must be known before unravel dispatch".into());
    }
    let Some(Structure::Record(fields, extension)) = sub.head_normal(companion) else {
        return Err("a companion must be a closed { forward, unravel } record".into());
    };
    let (fields, extension) = crate::interface::instance::flatten_record(sub, fields, extension);
    if extension.is_some() {
        return Err("a companion must have a closed record type".into());
    }
    if fields.len() != 2 || !fields.contains_key("forward") || !fields.contains_key("unravel") {
        return Err("a companion must have exactly forward and unravel fields".into());
    }
    if !predicate::same(sub, fields["forward"], forward) {
        return Err("companion forward type does not match the callback argument".into());
    }
    let Some(Structure::Fn(state, rest)) = sub.head_normal(fields["unravel"]) else {
        return Err("companion unravel field must accept a Feed state".into());
    };
    let arity = match sub.head_normal(state) {
        Some(Structure::Record(..)) => 1,
        Some(Structure::Tuple(states)) if states.len() >= 2 => states.len(),
        _ => return Err("companion needs one Feed per callback argument".into()),
    };
    let mut output = forward;
    let mut inputs = Vec::with_capacity(arity);
    let mut states = Vec::with_capacity(arity);
    for _ in 0..arity {
        let Some(Structure::Fn(input, rest)) = sub.head_normal(output) else {
            return Err("companion feed arity exceeds the callback's arguments".into());
        };
        if matches!(sub.head_normal(input), Some(Structure::Fn(..))) {
            return Err("callbacks that accept functions are not supported yet".into());
        }
        inputs.push(input);
        let pinned = sub.fresh_bound(Structure::App(Ref::Builtin("Maybe".into()), vec![input]));
        states.push(sub.fresh_bound(Structure::Record(
            BTreeMap::from([("value".into(), input), ("pinned".into(), pinned)]),
            None,
        )));
        output = rest;
    }
    if matches!(sub.head_normal(output), Some(Structure::Fn(..))) {
        return Err("companion must cover every argument and return a non-function result".into());
    }
    let (input, expected) = if arity == 1 {
        (inputs[0], states[0])
    } else {
        (
            sub.fresh_bound(Structure::Tuple(inputs)),
            sub.fresh_bound(Structure::Tuple(states)),
        )
    };
    if !predicate::same(sub, state, expected) {
        return Err("companion input states must match its forward arguments in order".into());
    }
    let Some(Structure::Fn(output_constraint, result)) = sub.head_normal(rest) else {
        return Err("companion unravel field must accept an output constraint".into());
    };
    let Some(Structure::App(head, args)) = sub.head_normal(result) else {
        return Err("companion unravel field must return Unravelled sIn".into());
    };
    if head != Ref::Builtin("Unravelled".into()) || args.len() != 1 {
        return Err("companion unravel field must return Unravelled sIn".into());
    }
    let input_constraint = args[0];
    if arity > 1
        && !matches!(sub.head_normal(input_constraint), Some(Structure::Tuple(ref requests)) if requests.len() == arity)
    {
        return Err("companion must return one request per callback argument".into());
    }
    let (input_validation, output_validation) = if generic {
        let input_pair = predicate::pack(sub, vec![input_constraint, input]);
        let output_pair = predicate::pack(sub, vec![output_constraint, output]);
        (
            resolve(sub, table, &HashMap::new(), "Constriction", input_pair)
                .map_err(|e| format!("callback input validator: {e:?}"))?,
            resolve(sub, table, &HashMap::new(), "Constriction", output_pair)
                .map_err(|e| format!("callback output validator: {e:?}"))?,
        )
    } else {
        validators(
            sub,
            table,
            &HashMap::new(),
            input,
            output,
            input_constraint,
            output_constraint,
        )?
    };
    Ok(CallbackContract {
        argument,
        arity,
        companion,
        input_constraint,
        output_constraint,
        input_validation,
        output_validation,
    })
}
fn validators(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    input: TypeVarId,
    output: TypeVarId,
    s_in: TypeVarId,
    s_out: TypeVarId,
) -> Result<(Evidence, Evidence), String> {
    let in_pair = predicate::pack(sub, vec![s_in, input]);
    let out_pair = predicate::pack(sub, vec![s_out, output]);
    Ok((
        complete(sub, table, given, "Constriction", in_pair)?,
        complete(sub, table, given, "Constriction", out_pair)?,
    ))
}
fn complete(
    sub: &mut Substitution,
    table: &InstanceTable,
    given: &HashMap<TypeVarId, HashSet<String>>,
    interface: &str,
    ty: TypeVarId,
) -> Result<Evidence, String> {
    match resolve(sub, table, given, interface, ty) {
        Ok(evidence) if evidence.residuals().is_empty() => Ok(evidence),
        _ => Err(format!(
            "missing {interface} evidence for {}",
            print_type(sub, ty)
        )),
    }
}
fn closed(sub: &mut Substitution, ty: TypeVarId) -> bool {
    match sub.head_normal(ty) {
        Some(s) => structure_children(&s).into_iter().all(|t| closed(sub, t)),
        None => false,
    }
}
