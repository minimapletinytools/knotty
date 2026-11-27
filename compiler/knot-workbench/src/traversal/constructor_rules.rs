//! Constructor policies use the same checked contracts and execution as calls.
use super::*;

/// An ordinary unravel rule attached to a constructor. Multiple entries keep
/// registration order. Source-level attachment syntax is still deferred.
#[derive(Clone, Debug)]
pub struct ConstructorRule {
    pub constructor: GlobalName,
    pub expression: String,
}

impl Walker<'_> {
    pub(super) fn register_constructor_rules(&mut self) -> Result<(), EditError> {
        for rule in self.constructor_rules {
            let arity = self.constructor_arity(&rule.constructor)?;
            if arity == 0 {
                return unsupported(
                    "constructor rules require fields; nullary values use materialization",
                );
            }
            self.probes.declarations.clear();
            let source = format!("rule =\n{}", indent(&rule.expression));
            let checked = self.probes.check(&source)?;
            let scheme = checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel("rule".into()))
                .unwrap();
            let mut sub = checked.substitution.clone();
            let Some(knot_checker::ty::Structure::Fn(state, _)) = sub.head_normal(scheme.ty) else {
                return unsupported("constructor rule needs field feeds");
            };
            if !valid_feed_shape(&mut sub, state, arity) {
                return unsupported("constructor rule needs one valid Feed per field");
            }
            let feeds = (0..arity).map(|i| format!("feed{i}")).collect::<Vec<_>>();
            let values = feeds
                .iter()
                .map(|f| format!("{f}.value"))
                .collect::<Vec<_>>();
            let requests = (0..arity)
                .map(|i| format!("request{i}"))
                .collect::<Vec<_>>();
            let value = if rule.constructor.module == ["Prelude"] && rule.constructor.name == ":" {
                format!("({} : {})", values[0], values[1])
            } else {
                format!(
                    "({} {})",
                    global_source(&rule.constructor),
                    values.join(" ")
                )
            };
            let checks = requests
                .iter()
                .zip(&values)
                .map(|(r, v)| format!("fits {r} {v}"))
                .collect::<Vec<_>>()
                .join(" && ");
            self.probes.check(&format!("{source}validate {} request = case rule {} request of\n  Snag -> True\n  Splice -> True\n  Unravel ({}) -> fits request {value} && {checks}\n", group(&feeds), group(&feeds), group(&requests)))?;
        }
        Ok(())
    }

    pub(super) fn select_constructor_rule(
        &mut self,
        site: &Site,
        constructor: &GlobalName,
        args: &[Site],
        request_ty: &Ty,
    ) -> Result<Option<(String, Ty)>, EditError> {
        let mut rules = self
            .constructor_rules
            .iter()
            .filter(|rule| &rule.constructor == constructor)
            .map(|rule| rule.expression.clone())
            .collect::<Vec<_>>();
        if rules.is_empty() {
            return Ok(None);
        }
        // A polymorphic constructor can have rules for distinct concrete field
        // types. Filter only forward-specialization mismatches before dispatch.
        let feeds = args
            .iter()
            .map(|a| candidates::feed_type(a.ty.as_ref().unwrap()).source())
            .collect::<Vec<_>>();
        let mut declarations = format!("states :: {}\nstates = undefined\n", group(&feeds));
        for (i, rule) in rules.iter().enumerate() {
            declarations.push_str(&format!("rule{i} =\n{}", indent(rule)));
        }
        self.probes.declarations.clear();
        let checked = self.probes.check(&declarations)?;
        let states = checked
            .schemes
            .get(&knot_checker::solve::SchemeKey::TopLevel("states".into()))
            .unwrap()
            .ty;
        rules = rules
            .into_iter()
            .enumerate()
            .filter_map(|(i, rule)| {
                let scheme = checked
                    .schemes
                    .get(&knot_checker::solve::SchemeKey::TopLevel(format!(
                        "rule{i}"
                    )))
                    .unwrap();
                let mut sub = checked.substitution.clone();
                let (ty, _) = knot_checker::solve::instantiate(&mut sub, scheme);
                let Some(knot_checker::ty::Structure::Fn(state, _)) = sub.head_normal(ty) else {
                    return Some(rule);
                };
                if !valid_feed_shape(&mut sub, state, args.len()) {
                    return Some(rule); // Let normal registration report malformed contracts.
                }
                if knot_checker::unify::unify(&mut sub, state, states).is_ok() {
                    Some(rule)
                } else {
                    None
                }
            })
            .collect();
        if rules.is_empty() {
            return Ok(None);
        }
        self.select_field_rule(site, args, request_ty, &rules)?
            .map(Some)
            .ok_or_else(|| EditError::Unsupported("no compatible unravel annotation".into()))
    }

    pub(super) fn select_list_rule(
        &mut self,
        site: &Site,
        constructor: &GlobalName,
        args: &[Site],
        request: &Ty,
    ) -> Result<Option<(String, Ty)>, EditError> {
        if constructor.module != ["Prelude"] || constructor.name != ":" {
            return Ok(None);
        }
        self.select_field_rule(site, args, request, &["Prelude.unravelListCons".into()])
    }

    pub(super) fn select_field_rule(
        &mut self,
        site: &Site,
        args: &[Site],
        request_ty: &Ty,
        rules: &[String],
    ) -> Result<Option<(String, Ty)>, EditError> {
        let output = site
            .ty
            .as_ref()
            .ok_or_else(|| EditError::Internal("constructor output type".into()))?;
        let inputs = args
            .iter()
            .map(|arg| {
                arg.ty
                    .clone()
                    .ok_or_else(|| EditError::Internal("constructor argument type".into()))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let input = if inputs.len() == 1 {
            inputs[0].clone()
        } else {
            Ty::Tuple(inputs)
        };
        let mut source = format!(
            "frame :: ({}, {})\nframe = undefined\nrequest :: {}\nrequest = undefined\n",
            output.source(),
            input.source(),
            request_ty.source()
        );
        for (i, rule) in rules.iter().enumerate() {
            source.push_str(&format!("rule{i} =\n{}", indent(rule)));
        }
        self.probes.declarations.clear();
        let checked = self.probes.check(&source)?;
        let plan = call_frame::CallPlan::new(checked, args.len())?;
        Ok(plan
            .try_select(rules.len())?
            .map(|(selected, input)| (rules[selected].clone(), input)))
    }
}

fn valid_feed_shape(
    sub: &mut knot_checker::var::Substitution,
    state: knot_checker::var::TypeVarId,
    arity: usize,
) -> bool {
    use knot_checker::ty::Structure;
    let states = if arity == 1 {
        vec![state]
    } else if let Some(Structure::Tuple(states)) = sub.head_normal(state) {
        if states.len() != arity {
            return false;
        }
        states
    } else {
        return false;
    };
    states.into_iter().all(|state| {
        let Some(Structure::Record(fields, None)) = sub.head_normal(state) else {
            return false;
        };
        let (Some(value), Some(pinned)) = (fields.get("value"), fields.get("pinned")) else {
            return false;
        };
        if fields.len() != 2 {
            return false;
        }
        let expected = sub.fresh_bound(Structure::App(Ref::Builtin("Maybe".into()), vec![*value]));
        knot_checker::predicate::same(sub, *pinned, expected)
    })
}

/// Built-in list constructors are syntax; their reverse policy is registered
/// exactly like declared nominal constructors.
pub(super) fn constructor_pattern(constructor: &GlobalName, names: &[String]) -> String {
    if constructor.module == ["Prelude"] && constructor.name == ":" {
        format!("{} : {}", names[0], names[1])
    } else {
        format!("{} {}", global_source(constructor), names.join(" "))
    }
}
impl Walker<'_> {
    pub(super) fn constructor_arity(&self, constructor: &GlobalName) -> Result<usize, EditError> {
        if constructor.module == ["Prelude"] && constructor.name == ":" {
            return Ok(2);
        }
        self.snapshot
            .graph
            .modules
            .iter()
            .map(|m| m.program.as_ref())
            .chain(self.bundled.iter())
            .find(|p| p.canonical.name == constructor.module)
            .and_then(|p| {
                p.canonical.decls.iter().find_map(|d| match &d.node {
                    CDecl::TypeDecl(_, _, variants, _) => variants
                        .iter()
                        .find(|(name, _)| name == &constructor.name)
                        .map(|(_, fields)| fields.len()),
                    _ => None,
                })
            })
            .ok_or_else(|| EditError::Unsupported("decomposer needs a declared constructor".into()))
    }
}

pub(super) fn exact_constructor_rule(constructor: &GlobalName, types: &[Ty]) -> String {
    let names = (0..types.len())
        .map(|i| format!("field{i}"))
        .collect::<Vec<_>>();
    let requests = names
        .iter()
        .map(|n| format!("Just {n}"))
        .collect::<Vec<_>>();
    let skipped = vec!["Nothing".to_owned(); names.len()];
    format!("\\_ request -> case request of\n  Nothing -> Unravel ({})\n  Just candidate -> case candidate of\n    {} -> Unravel ({})\n    _ -> Splice", group(&skipped), constructor_pattern(constructor, &names), group(&requests))
}
