//! Shared checked application metadata for source routing and body compilation.
use super::*;
use knot_checker::{program::TypedProgram, solve::SchemeKey, ty::Scheme, var::TypeVarId};

/// Revision-owned type/rule metadata, with no runtime handles or source sites.
/// Retained in the bounded checked-query cache, never executed as helper code.
pub(super) struct CallPlan {
    pub checked: Arc<TypedProgram>,
    pub arguments: Vec<Ty>,
    pub output: Ty,
    inputs: Vec<TypeVarId>,
    output_id: TypeVarId,
    selected: std::sync::OnceLock<(usize, Ty)>,
}
impl CallPlan {
    pub(super) fn new(checked: Arc<TypedProgram>, arity: usize) -> Result<Self, EditError> {
        let frame = checked
            .schemes
            .get(&SchemeKey::TopLevel("frame".into()))
            .ok_or_else(|| EditError::Internal("missing call frame".into()))?
            .ty;
        let mut sub = checked.substitution.clone();
        let Some(knot_checker::ty::Structure::Tuple(frame)) = sub.head_normal(frame) else {
            return Err(EditError::Internal("frame type".into()));
        };
        let inputs = if arity == 1 {
            vec![frame[1]]
        } else if let Some(knot_checker::ty::Structure::Tuple(inputs)) = sub.head_normal(frame[1]) {
            inputs
        } else {
            return Err(EditError::Internal("argument types".into()));
        };
        if inputs.len() != arity {
            return Err(EditError::Internal("call frame arity".into()));
        }
        let arguments = inputs
            .iter()
            .map(|ty| Ty::read(&mut sub, *ty))
            .collect::<Result<_, _>>()?;
        let output = Ty::read(&mut sub, frame[0])?;
        Ok(Self {
            checked,
            arguments,
            output,
            inputs,
            output_id: frame[0],
            selected: Default::default(),
        })
    }
}

pub(super) struct CheckedCall {
    pub fun: Site,
    pub args: Vec<Site>,
    pub actuals: Vec<String>,
    pub declarations: String,
    pub name: GlobalName,
    pub definition: Option<CFnDef>,
    pub rules: Vec<Site>,
    pub bodies: Vec<CallbackBody>,
    pub output: Ty,
    plan: Arc<CallPlan>,
}
impl CheckedCall {
    pub fn has_companions(&self) -> bool {
        (0..self.rules.len()).any(|i| {
            knot_checker::annotation::unravel::has_companion_prefix(
                &self.plan.checked.substitution,
                &self.plan.scheme(&format!("rule{i}")),
            )
        })
    }

    pub fn select(&self) -> Result<(usize, Ty), EditError> {
        self.plan.select(self.rules.len())
    }
}
impl CallPlan {
    fn scheme(&self, name: &str) -> Scheme {
        self.checked
            .schemes
            .get(&SchemeKey::TopLevel(name.into()))
            .unwrap()
            .clone()
    }
    pub(super) fn select(&self, rule_count: usize) -> Result<(usize, Ty), EditError> {
        self.try_select(rule_count)?
            .ok_or_else(|| EditError::Unsupported("no compatible unravel annotation".into()))
    }
    pub(super) fn try_select(&self, rule_count: usize) -> Result<Option<(usize, Ty)>, EditError> {
        if let Some(selected) = self.selected.get() {
            return Ok(Some(selected.clone()));
        }
        let rules = (0..rule_count)
            .map(|i| self.scheme(&format!("rule{i}")))
            .collect::<Vec<_>>();
        let selection = knot_checker::annotation::unravel::register_and_select(
            &self.checked.substitution,
            &self.checked.instances_table,
            &rules,
            &self.inputs,
            self.output_id,
            self.scheme("request").ty,
        )
        .map_err(|e| EditError::Check(format!("{e:?}")))?
        .selected;
        let Some(selected) = selection else {
            return Ok(None);
        };
        let input = Ty::read(
            &mut selected.substitution.clone(),
            selected.input_constraint,
        )?;
        let result = (selected.index, input);
        let _ = self.selected.set(result.clone());
        Ok(Some(result))
    }
}
impl Walker<'_> {
    pub(super) fn application_parts(
        &mut self,
        site: &Site,
        ambient: &str,
    ) -> Result<(Site, Vec<Site>), EditError> {
        let (mut fun, mut args) = flatten(site);
        if let CExpr::Var(Ref::Builtin(op)) = &fun.expr.node {
            if op == "|>" && args.len() == 2 {
                let right = args.pop().unwrap();
                let left = args.pop().unwrap();
                (fun, args) = flatten(&right);
                args.push(left);
            } else if op == "<|" && args.len() == 2 {
                let right = args.pop().unwrap();
                let left = args.pop().unwrap();
                (fun, args) = flatten(&left);
                args.push(right);
            }
        }
        self.resolve_function(site, fun, args, ambient)
    }
    pub(super) fn checked_call(
        &mut self,
        site: &Site,
        fun: Site,
        args: Vec<Site>,
        request_type: &Ty,
        request: &str,
        prefix: &str,
    ) -> Result<CheckedCall, EditError> {
        let mut args = args
            .into_iter()
            .map(|a| self.resolve_local(a))
            .collect::<Result<Vec<_>, _>>()?;
        let (fnsource, mut helpers) = self.render_site_typed(&fun, "twineCallableType")?;
        let mut metadata_helpers = helpers.clone();
        let mut actuals = Vec::new();
        for (i, arg) in args.iter().enumerate() {
            let (source, declarations) =
                self.render_site_typed(arg, &format!("twineArgument{i}Type"))?;
            let known = arg.ty.as_ref().or_else(|| {
                if arg.recipe.is_none() && arg.projections.is_empty() {
                    if let CExpr::Var(reference_) = &arg.expr.node {
                        return self
                            .probes
                            .monomorphic_global(&reference(reference_, &arg.module));
                    }
                }
                None
            });
            if let Some(ty) = known {
                metadata_helpers.push_str(&format!(
                    "provided{i} :: {}\nprovided{i} = undefined\n",
                    ty.source()
                ));
            } else {
                // Unknown/polymorphic arguments still need contextual inference.
                // These helpers remain metadata only; actuals are kept separately.
                metadata_helpers.push_str(&declarations);
                metadata_helpers.push_str(&format!("provided{i} =\n{}", indent(&source)));
            }
            helpers.push_str(&declarations);
            helpers.push_str(&format!(
                "{}provided{i} =\n{}",
                arg.ty
                    .as_ref()
                    .map(|t| format!("provided{i} :: {}\n", t.source()))
                    .unwrap_or_default(),
                indent(&source)
            ));
            actuals.push(source);
        }
        let provided = (0..args.len())
            .map(|i| format!("provided{i}"))
            .collect::<Vec<_>>();
        let params = (0..args.len()).map(|i| format!("a{i}")).collect::<Vec<_>>();
        let applied = format!("fun {}", params.join(" "));
        let output = site
            .ty
            .as_ref()
            .map(|t| format!("hold :: {} -> {}\nhold x = x\n", t.source(), t.source()))
            .unwrap_or_else(|| "hold x = x\n".into());
        let mut declarations = format!("{prefix}{helpers}{output}request :: {}\nrequest = {request}\nframe = (\\fun {} -> (hold ({applied}), {})) ({fnsource}) {}\n", request_type.source(), params.join(" "), group(&params), provided.join(" "));
        let mut metadata = format!("{prefix}{metadata_helpers}{output}request :: {}\nrequest = undefined\nframe = (\\fun {} -> (hold ({applied}), {})) ({fnsource}) {}\n", request_type.source(), params.join(" "), group(&params), provided.join(" "));
        self.probes.declarations.clear();
        let initial = self.probes.call_plan(&metadata, args.len())?;
        let (name, definition) = match &fun.expr.node {
            CExpr::Ctor(r) => (reference(r, &fun.module), None),
            CExpr::Var(r)
                if reference(r, &fun.module)
                    == GlobalName {
                        module: vec!["Prelude".into()],
                        name: ":".into(),
                    }
                    && args.len() == 2 =>
            {
                (reference(r, &fun.module), None)
            }
            CExpr::Var(r) => {
                let name = reference(r, &fun.module);
                let (name, f) = if let Some(f) = self.definition(&name) {
                    (name, f)
                } else {
                    self.method_definition(&initial.checked)?
                };
                (name, Some(f))
            }
            CExpr::Lambda(params, body) => (
                GlobalName {
                    module: fun.module.clone(),
                    name: "lambda".into(),
                },
                Some(CFnDef {
                    name: "lambda".into(),
                    signature: None,
                    params: params.clone(),
                    body: *body.clone(),
                    annotations: vec![],
                }),
            ),
            _ => return unsupported("no callback source body"),
        };
        let rules = definition
            .iter()
            .flat_map(|f| f.annotations.iter())
            .filter(|a| a.key == "unravel")
            .map(|rule| Site {
                recipe: None,
                candidate_source: false,
                via: None,
                synthetic: false,
                projections: vec![],
                module: name.module.clone(),
                expr: rule.value.clone(),
                scope: 0,
                locked: true,
                fixed: false,
                ty: None,
            })
            .collect::<Vec<_>>();
        let mut bodies = Vec::new();
        for annotation in definition
            .iter()
            .flat_map(|f| &f.annotations)
            .filter(|a| a.key == "callback_body")
        {
            let CExpr::Tuple(parts) = &annotation.value.node else {
                return unsupported("callback_body expects (argument index, contract witness)");
            };
            let [position, contract] = parts.as_slice() else {
                return unsupported("callback_body expects (argument index, contract witness)");
            };
            let CExpr::IntLit(position) = &position.node else {
                return unsupported(
                    "callback_body argument index must be a nonnegative integer literal",
                );
            };
            let argument = position.to_string().parse::<usize>().map_err(|_| {
                EditError::Unsupported("invalid callback_body argument index".into())
            })?;
            if bodies.iter().any(|p: &CallbackBody| p.argument == argument) {
                return unsupported("duplicate callback_body position");
            }
            let contract = Site {
                recipe: None,
                candidate_source: false,
                via: None,
                synthetic: true,
                projections: vec![],
                module: name.module.clone(),
                expr: contract.clone(),
                scope: 0,
                locked: true,
                fixed: true,
                ty: None,
            };
            bodies.push(CallbackBody {
                function: name.clone(),
                argument,
                contract: self.source(&contract)?,
            });
        }
        // Explicit embedding registrations override source defaults at the same
        // position. The selected method's annotations, not its spelling, supply
        // defaults for dictionary methods.
        bodies.retain(|p| {
            !self
                .callback_factories
                .iter()
                .any(|f| f.function == name && f.argument == p.argument)
                && !self
                    .callback_bodies
                    .iter()
                    .any(|f| f.function == name && f.argument == p.argument)
        });
        bodies.extend(
            self.callback_bodies
                .iter()
                .filter(|p| p.function == name)
                .cloned(),
        );
        for (i, rule) in rules.iter().enumerate() {
            let rule = format!("rule{i} =\n{}", indent(&self.source(rule)?));
            declarations.push_str(&rule);
            metadata.push_str(&rule);
        }
        let plan = if rules.is_empty() {
            initial
        } else {
            self.probes.call_plan(&metadata, args.len())?
        };
        for (arg, ty) in args.iter_mut().zip(&plan.arguments) {
            arg.ty = Some(ty.clone());
        }
        Ok(CheckedCall {
            fun,
            args,
            actuals,
            declarations,
            name,
            definition,
            rules,
            bodies,
            output: plan.output.clone(),
            plan,
        })
    }
}
