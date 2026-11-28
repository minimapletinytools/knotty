//! Compile a declared callback-body reverse contract into ordinary checked Knot.
//! Generated helpers return candidates plus guards, never source patches.
use super::*;
mod aggregates;
mod branches;
mod higher_order;
mod lenses;
mod multi;

/// `contract` is a type witness taking the actual callback and outer request.
/// Generated companions return one Maybe request per ordinary input. The witness value is never executed.
#[derive(Clone, Debug)]
pub struct CallbackBody {
    pub function: GlobalName,
    pub argument: usize,
    pub contract: String,
}
pub(super) struct GeneratedBody {
    pub factory: String,
    pub declarations: String,
}
struct Compiler<'w, 's> {
    walker: &'w mut Walker<'s>,
    input: Ty,
    ambient: String,
    prefix: String,
    symbol: String,
    declarations: String,
    nodes: usize,
    multi: Option<multi::Inputs>,
    dependencies: BTreeMap<String, Vec<usize>>,
}
struct Node {
    forward: String,
    unravel: String,
    depends: bool,
}
fn optional(ty: Ty) -> Ty {
    Ty::Named("Maybe".into(), vec![ty])
}
pub(super) fn callback_shape(forward: &Ty) -> Result<(Vec<Ty>, Ty), EditError> {
    let mut inputs = Vec::new();
    let mut output = forward;
    while let Ty::Function(input, rest) = output {
        if matches!(**input, Ty::Function(..)) {
            return unsupported("callbacks that accept functions are not supported yet");
        }
        inputs.push(*input.clone());
        output = rest;
    }
    if inputs.is_empty() {
        return unsupported("body companion needs a callback function");
    }
    Ok((inputs, output.clone()))
}
impl Walker<'_> {
    pub(super) fn generate_body_in(
        &mut self,
        callback: &Site,
        output_request: &Ty,
        ambient: &str,
    ) -> Result<GeneratedBody, EditError> {
        if self.body_depth >= 8 {
            return unsupported("callback body compilation exceeded 8 nested companions");
        }
        self.body_depth += 1;
        let result = self.compile_body(callback, output_request, ambient);
        self.body_depth -= 1;
        result
    }
    fn compile_body(
        &mut self,
        callback: &Site,
        output_request: &Ty,
        ambient: &str,
    ) -> Result<GeneratedBody, EditError> {
        if matches!(&callback.ty, Some(Ty::Function(_, output)) if matches!(**output, Ty::Function(..)))
        {
            return self.compile_multi_body(callback, output_request, ambient);
        }
        let Some(Ty::Function(input, output)) = &callback.ty else {
            return unsupported("body companion needs a unary callback");
        };
        let mut prefix = format!("twineBody{}x{}", self.visits, self.scopes.len());
        while self
            .snapshot
            .sources()
            .values()
            .any(|source| source.contains(&prefix))
        {
            prefix.push('x');
        }
        let symbol = format!("{prefix}Argument");
        let callback = self.freeze(callback.clone())?;
        let mut argument = callback.clone();
        argument.module = vec![];
        argument.scope = 0;
        argument.expr = Spanned::new(
            callback.expr.span,
            CExpr::Var(Ref::TopLevel(symbol.clone())),
        );
        argument.ty = Some(*input.clone());
        argument.fixed = false;
        argument.synthetic = true;
        let scope = self.scopes.len();
        self.scopes.push(BTreeMap::from([
            ("bodyCallback".into(), callback.clone()),
            ("bodyArgument".into(), argument),
        ]));
        let span = callback.expr.span;
        let application = Site {
            recipe: None,
            candidate_source: false,
            via: None,
            lash: None,
            cleat: None,
            synthetic: true,
            projections: vec![],
            module: callback.module.clone(),
            expr: Spanned::new(
                span,
                CExpr::App(
                    Box::new(Spanned::new(
                        span,
                        CExpr::Var(Ref::Local("bodyCallback".into())),
                    )),
                    Box::new(Spanned::new(
                        span,
                        CExpr::Var(Ref::Local("bodyArgument".into())),
                    )),
                ),
            ),
            scope,
            locked: true,
            fixed: false,
            ty: Some(*output.clone()),
        };
        let mut compiler = Compiler {
            walker: self,
            input: *input.clone(),
            ambient: ambient.into(),
            prefix,
            symbol,
            declarations: String::new(),
            nodes: 0,
            multi: None,
            dependencies: BTreeMap::new(),
        };
        let root = compiler.emit(application, output_request)?;
        // The solve stays in Maybe; the factory lifts it into a rule's answer.
        let outer = format!("{}Factory", compiler.prefix);
        let factory = format!("{outer}Solve");
        compiler.declarations.push_str(&format!("{outer} :: ({} -> {}) -> {} -> {} -> Unravelled (Maybe {})\n{outer} actual input request = orSnag ({factory} actual input request)\n", input.source(), output.source(), candidates::feed_type(input).source(), output_request.source(), input.source()));
        compiler.declarations.push_str(&format!("{factory} :: ({} -> {}) -> {} -> {} -> Maybe (Maybe {})\n{factory} actual input request = if fits request (actual input.value) then Just Nothing else do\n  (candidate, guard) <- {} input input.value request\n  if guard candidate && fits request (actual candidate) && pinAllows input (Just candidate) then Just (Just candidate) else Nothing\n", input.source(), output.source(), candidates::feed_type(input).source(), output_request.source(), input.source(), root.unravel));
        // Check every generated signature/evidence before any reverse execution.
        compiler.walker.probes.declarations = ambient.into();
        compiler.walker.probes.check(&compiler.declarations)?;
        Ok(GeneratedBody {
            factory: outer,
            declarations: compiler.declarations,
        })
    }
    pub(super) fn pin_typed_policy_expression(
        &mut self,
        expression: &str,
        ty: &Ty,
    ) -> Result<(), EditError> {
        self.probes.declarations.clear();
        let signature = format!("bodyPolicy :: {}\n", ty.source());
        let checked = self
            .probes
            .check(&format!("{signature}bodyPolicy =\n{}", indent(expression)))?;
        let definition = checked
            .canonical
            .decls
            .iter()
            .find_map(|d| match &d.node {
                CDecl::Fn(f) if f.name == "bodyPolicy" => Some(f),
                _ => None,
            })
            .unwrap();
        self.pin_source_dependencies(Site {
            recipe: None,
            candidate_source: false,
            via: None,
            lash: None,
            cleat: None,
            synthetic: true,
            projections: vec![],
            module: checked.canonical.name.clone(),
            expr: definition.body.clone(),
            scope: 0,
            locked: true,
            fixed: true,
            ty: Some(ty.clone()),
        })
    }
}
impl Compiler<'_, '_> {
    // Type queries inspect original callback expressions, not the executable
    // helpers emitted so far. Their environment needs only the enclosing
    // context and the concrete callback input. Including `declarations` here
    // repeatedly checks and retains growing prefixes of the reverse program.
    // Each rendered expression supplies its own capture/type identities; the
    // complete executable body is still checked at the end of compile_body.
    fn metadata(&self) -> String {
        let mut result = format!(
            "{}{} :: {}\n{} = undefined\n",
            self.ambient,
            self.symbol,
            self.input.source(),
            self.symbol
        );
        if let Some(inputs) = &self.multi {
            for (symbol, ty) in inputs.symbols.iter().zip(&inputs.types) {
                result.push_str(&format!(
                    "{symbol} :: {}\n{symbol} = undefined\n",
                    ty.source()
                ));
            }
        }
        result
    }
    fn forward(&mut self, site: &Site, name: &str) -> Result<(Ty, bool), EditError> {
        let (source, helpers) = self
            .walker
            .render_site_typed(site, &format!("{name}Type"))?;
        self.declarations.push_str(&helpers);
        let ty = if let Some(ty) = &site.ty {
            ty.clone()
        } else {
            self.walker.probes.declarations = format!("{}{helpers}", self.metadata());
            let checked = self
                .walker
                .probes
                .check(&format!("nodeValue =\n{}", indent(&source)))?;
            let scheme = checked
                .schemes
                .get(&knot_checker::solve::SchemeKey::TopLevel(
                    "nodeValue".into(),
                ))
                .unwrap();
            Ty::read(&mut checked.substitution.clone(), scheme.ty)?
        };
        let source = self.bind_multi_roots(&source);
        self.declarations.push_str(&format!(
            "{name} :: {} -> {}\n{name} {} =\n{}",
            self.input.source(),
            ty.source(),
            self.symbol,
            indent(&source)
        ));
        let dependency = self.dependency_source(site)?;
        let depends = if let Some(inputs) = &self.multi {
            let roots = inputs
                .symbols
                .iter()
                .enumerate()
                .filter_map(|(i, symbol)| dependency.contains(symbol).then_some(i))
                .collect::<Vec<_>>();
            let depends = !roots.is_empty();
            self.dependencies.insert(name.into(), roots);
            depends
        } else {
            dependency.contains(&self.symbol)
        };
        Ok((ty, depends))
    }
    /// A field selected from a visible construction depends only on that field,
    /// not on unused siblings in its rendered projection. The full forward
    /// helper still checks pattern matches and keeps their runtime guards.
    fn dependency_source(&self, site: &Site) -> Result<String, EditError> {
        let mut value = site.clone();
        for _ in 0..128 {
            let path = std::mem::take(&mut value.projections);
            if !path.is_empty() {
                value.ty = path[0].input.clone();
            }
            value = self.walker.resolve_local(value)?;
            value.projections.extend(path);
            if value.projections.is_empty() {
                return self.walker.source(&value);
            }
            let first = value.projections.remove(0);
            let rest = std::mem::take(&mut value.projections);
            if matches!(first.kind, ProjectionKind::Match { .. }) {
                value.projections = rest;
                continue;
            }
            let Some(mut selected) = projections::direct(&value, &first.kind) else {
                return self.walker.source(site);
            };
            selected.projections.extend(rest);
            value = selected;
        }
        unsupported("callback projection dependency exceeded 128 levels")
    }
    fn emit(&mut self, site: Site, request: &Ty) -> Result<Node, EditError> {
        if field_requests::payload(request).is_some() {
            return self.field(site, request);
        }
        self.nodes += 1;
        self.walker.body_nodes += 1;
        if self.walker.body_nodes > 128 {
            return unsupported("callback body compilation exceeded 128 nodes");
        }
        let mut site = self.walker.resolve_local(site)?;
        let forward = format!("{}Forward{}", self.prefix, self.nodes);
        let unravel = format!("{}Unravel{}", self.prefix, self.nodes);
        let (value, depends) = self.forward(&site, &forward)?;
        site.ty = Some(value.clone());
        let result = Ty::Tuple(vec![
            self.input.clone(),
            Ty::Function(
                Box::new(self.input.clone()),
                Box::new(Ty::Named("Bool".into(), vec![])),
            ),
        ]);
        let body = if !depends {
            format!("if fits request ({forward} input.value) then Just (current, \\candidate -> fits request ({forward} candidate)) else Nothing")
        } else if let Some((expression, output)) = self.walker.adapter_policy(&value, request) {
            self.translated(&site, &forward, expression, request, output)?
        } else if site.projections.is_empty()
            && site.module.is_empty()
            && matches!(&site.expr.node, CExpr::Var(Ref::TopLevel(name)) if name == &self.symbol)
        {
            let selector = self.walker.candidate_policy(&value, request);
            self.walker.pin_typed_policy_expression(
                &selector,
                &candidates::policy_type(&value, request, value.clone()),
            )?;
            format!("if fits request current then Just (current, \\value -> fits request value) else\n  do\n    candidate <- ({selector}) input request\n    pure (candidate, \\value -> fits request value)")
        } else if !site.projections.is_empty() || self.multi_root(&site).is_some() {
            self.projected_input(&site, request, &forward)?
        } else {
            match &site.expr.node {
                CExpr::Annotated(_, child) => {
                    let mut child = site.child(child);
                    child.ty = Some(value);
                    let node = self.emit(child, request)?;
                    format!("{} input current request", node.unravel)
                }
                CExpr::Let(bindings, body) => {
                    let body = self.walker.let_site(&site, bindings, body)?;
                    let node = self.emit(body, request)?;
                    format!("{} input current request", node.unravel)
                }
                CExpr::If(condition, yes, no) => {
                    let mut condition = site.child(condition);
                    condition.ty = Some(Ty::Named("Bool".into(), vec![]));
                    let name = format!("{unravel}Condition");
                    self.forward(&condition, &name)?;
                    let mut yes = site.child(yes);
                    yes.ty = site.ty.clone();
                    let yes = self.emit(yes, request)?;
                    let mut no = site.child(no);
                    no.ty = site.ty.clone();
                    let no = self.emit(no, request)?;
                    format!("if {name} {} then\n  do\n    (candidate, guard) <- {} input current request\n    pure (candidate, \\value -> {name} value && guard value && fits request ({forward} value))\nelse\n  do\n    (candidate, guard) <- {} input current request\n    pure (candidate, \\value -> not ({name} value) && guard value && fits request ({forward} value))", self.current_value(), yes.unravel, no.unravel)
                }
                CExpr::Case(scrutinee, arms) => {
                    self.case_body(&site, scrutinee, arms, request, &forward)?
                }
                CExpr::FieldAccess(base, name) => {
                    let projected = site
                        .child(base)
                        .projected(ProjectionKind::Field(name.clone()), site.ty.clone());
                    let child = self.emit(projected, request)?;
                    format!("{} input current request", child.unravel)
                }
                CExpr::Tuple(_) | CExpr::Record(_) | CExpr::RecordUpdate(_, _) => {
                    self.product(&site, request, &forward)?
                }
                CExpr::List(_) => {
                    let fields = site.list_fields().ok_or_else(|| {
                        EditError::Unsupported("empty generated list has no editable fields".into())
                    })?;
                    self.constructed(
                        &site,
                        &GlobalName {
                            module: vec!["Prelude".into()],
                            name: ":".into(),
                        },
                        fields.into(),
                        request,
                        &forward,
                    )?
                }
                CExpr::Negate(child) => {
                    let expr = Spanned::new(
                        site.expr.span,
                        CExpr::App(
                            Box::new(Spanned::new(
                                site.expr.span,
                                CExpr::Var(Ref::Builtin("negate".into())),
                            )),
                            child.clone(),
                        ),
                    );
                    self.application(
                        &Site {
                            expr,
                            ..site.clone()
                        },
                        request,
                        &forward,
                    )?
                }
                CExpr::App(_, _) | CExpr::BinOp(_, _, _) => {
                    self.application(&site, request, &forward)?
                }
                _ => return unsupported("no generated body reverse route for this expression"),
            }
        };
        // Match the source walker's no-op semantics while retaining a guard for
        // siblings that may subsequently change this same input.
        let body = format!("if fits request ({forward} {}) then Just (current, \\value -> fits request ({forward} value)) else\n{}", self.current_value(), indent(&body));
        self.declarations.push_str(&format!(
            "{unravel} :: {} -> {} -> {} -> {}\n{unravel} input current request =\n{}",
            candidates::feed_type(&self.input).source(),
            self.input.source(),
            request.source(),
            optional(result).source(),
            indent(&body)
        ));
        Ok(Node {
            forward,
            unravel,
            depends,
        })
    }
    fn application(
        &mut self,
        site: &Site,
        request: &Ty,
        forward: &str,
    ) -> Result<String, EditError> {
        let (fun, args) = self.walker.application_parts(site, &self.metadata())?;
        let call =
            self.walker
                .checked_call(site, fun, args, request, "undefined", &self.metadata())?;
        let Some(definition) = &call.definition else {
            let typed_site = Site {
                ty: Some(call.output),
                ..site.clone()
            };
            return self.constructed(&typed_site, &call.name, call.args, request, forward);
        };
        if call.has_companions()
            || !call.bodies.is_empty()
            || self
                .walker
                .callback_factories
                .iter()
                .any(|p| p.function == call.name)
        {
            return self.higher_order(call, request, forward);
        }
        if call.rules.is_empty() {
            if definition.params.len() != call.args.len() {
                return unsupported(
                    "generated callback partial application needs a concrete source body",
                );
            }
            let scope = self.walker.scopes.len();
            self.walker
                .scopes
                .push(self.walker.scopes[call.fun.scope].clone());
            for (pattern, arg) in definition.params.iter().zip(call.args) {
                self.walker
                    .bind_pattern(&pattern.node, arg, scope, &call.name.module, true)?;
            }
            let body = Site {
                recipe: None,
                candidate_source: false,
                via: None,
                lash: None,
                cleat: None,
                synthetic: true,
                projections: vec![],
                module: call.name.module,
                expr: definition.body.clone(),
                scope,
                locked: true,
                fixed: false,
                ty: Some(call.output),
            };
            let child = self.emit(body, request)?;
            return Ok(format!("do\n  (candidate, guard) <- {} input current request\n  pure (candidate, \\value -> guard value && fits request ({forward} value))", child.unravel));
        }
        let (selected, input_requests) = call.select()?;
        let mut selected_rule = call.rules[selected].clone();
        selected_rule.ty = Some(candidates::written_rule_type(
            call.args.iter().map(|arg| arg.ty.clone().unwrap()),
            request,
            input_requests.clone(),
            call.shape().1,
        ));
        self.walker.pin_source_dependencies(selected_rule)?;
        let rule = call.with_feeds(&self.walker.source(&call.rules[selected])?);
        self.unravel_apply(&rule, call.args, input_requests, forward)
    }
    fn unravel_apply(
        &mut self,
        rule: &str,
        args: Vec<Site>,
        input_requests: Ty,
        forward: &str,
    ) -> Result<String, EditError> {
        let types = if args.len() == 1 {
            vec![input_requests]
        } else {
            let Ty::Tuple(types) = input_requests else {
                return Err(EditError::Internal("body rule input tuple".into()));
            };
            types
        };
        if types.len() != args.len() {
            return Err(EditError::Internal("body rule input arity".into()));
        }
        let mut children = Vec::new();
        for (arg, ty) in args.into_iter().zip(types) {
            children.push(self.emit(arg, &ty)?);
        }
        let states = children
            .iter()
            .map(|child| {
                let original = format!("{} {}", child.forward, self.current_value());
                let pin = if child.depends {
                    self.input_pin(&child.forward)
                } else {
                    format!("Just ({original})")
                };
                format!("{{ value = {original}, pinned = (\n{}\n) }}", indent(&pin))
            })
            .collect::<Vec<_>>();
        let requests = (0..children.len())
            .map(|i| format!("part{i}"))
            .collect::<Vec<_>>();
        // Generated code cannot replace the callback's shared body, so a
        // rule's Splice is a failure here, as Snag is.
        let mut body = format!(
            "do\n  {} <- unravelledToMaybe ((\n{}  ) {} request)\n",
            group(&requests),
            indent(&indent(rule)),
            group(&states)
        );
        for (i, child) in children.iter().enumerate() {
            let previous = if i == 0 {
                "current".into()
            } else {
                format!("candidate{}", i - 1)
            };
            body.push_str(&format!(
                "  (candidate{i}, guard{i}) <- {} input {previous} part{i}\n",
                child.unravel
            ));
        }
        let guards = (0..children.len())
            .map(|i| format!("guard{i} value"))
            .collect::<Vec<_>>()
            .join(" && ");
        body.push_str(&format!(
            "  pure (candidate{}, \\value -> {guards} && fits request ({forward} value))",
            children.len() - 1
        ));
        Ok(body)
    }
}
