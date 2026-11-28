//! Source companions retain the concrete callback and its lexical captures.
//! Checked call frames select exact dictionary evidence; ordinary companion
//! policies carry callback constraints.
use super::*;
use knot_checker::{
    ast::TExpr,
    program::{DictExpr, EvidenceSite, ResolvedReference, TypedProgram},
};

impl Walker<'_> {
    pub(super) fn retain_argument_pins(&mut self, args: &[Site]) -> Result<Vec<bool>, EditError> {
        // Rules cannot waive pins by returning an unconstrained request. Keep
        // the original pinned value as a separate final validation obligation.
        let pins = args
            .iter()
            .map(|a| self.is_pinned(a, 0))
            .collect::<Vec<_>>();
        for arg in args.iter().zip(&pins).filter_map(|(a, p)| p.then_some(a)) {
            // An empty synthetic construction has no editable fields or source
            // span of its own. Its pin holds by construction; do not require Eq
            // for an unused element type just to compare two empty shells.
            if arg.synthetic && arg.projections.is_empty() {
                let empty = match &arg.expr.node {
                    CExpr::List(parts) | CExpr::Tuple(parts) => parts.is_empty(),
                    CExpr::Record(fields) => fields.is_empty(),
                    _ => false,
                };
                if empty {
                    continue;
                }
            }
            let (request, ty) = if let Some(value) = self.solver_pin_value(arg)? {
                let reference = self.probes.reference(&value)?;
                let ty = Ty::Named("Maybe".into(), vec![Ty::from_value(value.value_type())]);
                self.probes.declarations.clear();
                self.probes
                    .evaluate_lazy(&format!("Just {reference}"), Some(&ty))?
            } else {
                let (source, helpers) = self.render_site_typed(arg, "twinePinType")?;
                self.probes.declarations = helpers;
                let ty = arg
                    .ty
                    .as_ref()
                    .map(|t| Ty::Named("Maybe".into(), vec![t.clone()]));
                let (request, ty) = self
                    .probes
                    .evaluate_lazy(&format!("Just (\n{}\n)", indent(&source)), ty.as_ref())?;
                (request, ty)
            };
            self.obligations.push((arg.clone(), request, ty));
        }
        Ok(pins)
    }

    pub(super) fn is_pinned(&self, s: &Site, depth: usize) -> bool {
        if s.fixed || depth > 128 || self.solver_pinned(s) {
            return true;
        }
        if !s.projections.is_empty() {
            return self.projected_pin(s, depth + 1);
        }
        if let CExpr::Var(Ref::Local(n)) = &s.expr.node {
            return self.scopes[s.scope]
                .get(n)
                .is_none_or(|a| self.is_pinned(a, depth + 1));
        }
        if !s.locked && s.module == self.snapshot.graph.entry().program.canonical.name {
            // Solving in passes: an input is pinned once every source value
            // it depends on has been decided.
            return self.stepping && self.all_decided(s, depth + 1);
        }
        let pinned = |e: &Spanned<CExpr>| self.is_pinned(&s.child(e), depth + 1);
        match &s.expr.node {
            CExpr::App(a, b) | CExpr::BinOp(_, a, b) => pinned(a) && pinned(b),
            CExpr::Annotated(_, e) | CExpr::Negate(e) | CExpr::FieldAccess(e, _) => pinned(e),
            CExpr::List(es) | CExpr::Tuple(es) => es.iter().all(pinned),
            CExpr::Record(fs) => fs.iter().all(|(_, e)| pinned(e)),
            CExpr::RecordUpdate(b, fs) => pinned(b) && fs.iter().all(|(_, e)| pinned(e)),
            CExpr::If(c, t, f) => pinned(c) && pinned(t) && pinned(f),
            CExpr::Let(bs, b) => pinned(b) && bs.iter().all(|(_, e)| pinned(e)),
            _ => true,
        }
    }
    pub(super) fn condition(&mut self, site: &Site) -> Result<bool, EditError> {
        self.probes.declarations = self.current_declaration(site)?;
        let result = self.probes.boolean("current")?;
        self.guards.push((
            site.clone(),
            if result {
                "current".into()
            } else {
                "not current".into()
            },
        ));
        Ok(result)
    }
    pub(super) fn resolve_local(&self, mut s: Site) -> Result<Site, EditError> {
        for _ in 0..128 {
            if !s.projections.is_empty() {
                return Ok(s);
            }
            match &s.expr.node {
                CExpr::Var(Ref::Local(n)) => {
                    let mut next = self.scopes[s.scope]
                        .get(n)
                        .cloned()
                        .ok_or_else(|| EditError::Unsupported(format!("missing capture {n}")))?;
                    if next.ty.is_none() {
                        next.ty = s.ty.clone();
                    }
                    next.fixed |= s.fixed;
                    s = next;
                }
                _ => return Ok(s),
            }
        }
        unsupported("cyclic callback capture")
    }
    pub(super) fn resolve_function(
        &mut self,
        context: &Site,
        mut fun: Site,
        mut args: Vec<Site>,
        ambient: &str,
    ) -> Result<(Site, Vec<Site>), EditError> {
        for _ in 0..128 {
            fun = self.resolve_local(fun)?;
            if fun.ty.is_none()
                && (!fun.projections.is_empty()
                    || matches!(
                        fun.expr.node,
                        CExpr::FieldAccess(..) | CExpr::Case(..) | CExpr::Let(..)
                    ))
            {
                self.type_callable(context, &mut fun, &mut args, ambient)?;
            }
            if !fun.projections.is_empty() {
                fun = self.focus_callable(fun)?;
                continue;
            }
            match &fun.expr.node {
                CExpr::App(_, _) | CExpr::BinOp(_, _, _) => {
                    let (f, mut prefix) = flatten(&fun);
                    if fun.locked {
                        for arg in &mut prefix {
                            arg.fixed = true;
                        }
                    }
                    prefix.extend(args);
                    fun = f;
                    args = prefix;
                }
                CExpr::Lambda(params, body) if args.len() > params.len() => {
                    fun = self.enter_callable_body(
                        context,
                        fun.clone(),
                        &mut args,
                        params,
                        body,
                        &fun.module,
                        ambient,
                    )?;
                }
                CExpr::Annotated(_, e) => {
                    fun = Site {
                        ty: fun.ty.clone(),
                        ..fun.child(e)
                    }
                }
                CExpr::FieldAccess(base, field) => {
                    fun = fun
                        .child(base)
                        .projected(ProjectionKind::Field(field.clone()), fun.ty.clone());
                }
                CExpr::Let(bindings, body) => fun = self.let_site(&fun, bindings, body)?,
                CExpr::Case(value, arms) => fun = self.case_site(&fun, value, arms)?,
                CExpr::If(c, t, f) => {
                    let taken = self.condition(&fun.child(c))?;
                    fun = Site {
                        ty: fun.ty.clone(),
                        ..fun.child(if taken { t } else { f })
                    };
                }
                CExpr::Var(r) => {
                    let g = reference(r, &fun.module);
                    if let Some(f) = self.definition(&g) {
                        if !f.params.is_empty()
                            && args.len() > f.params.len()
                            && f.annotations.iter().all(|a| a.key != "unravel")
                        {
                            fun = self.enter_callable_body(
                                context,
                                fun.clone(),
                                &mut args,
                                &f.params,
                                &f.body,
                                &g.module,
                                ambient,
                            )?;
                            continue;
                        }
                        // An annotation on a zero-parameter alias still belongs
                        // to that binding and must win before following its RHS.
                        if f.params.is_empty() && f.annotations.iter().all(|a| a.key != "unravel") {
                            fun = Site {
                                recipe: None,
                                candidate_source: false,
                                via: None,
                                lash: None,
                                cleat: None,
                                synthetic: false,
                                projections: vec![],
                                module: g.module,
                                expr: f.body,
                                scope: 0,
                                locked: fun.locked,
                                fixed: fun.fixed,
                                ty: fun.ty,
                            };
                            continue;
                        }
                    }
                    return Ok((fun, args));
                }
                _ => return Ok((fun, args)),
            }
        }
        unsupported("cyclic or deeply nested callback alias")
    }
    /// Enter a fully supplied prefix which returns another function. Its
    /// arguments retain their checked types and source activation; a captured
    /// partial-application argument was already marked fixed by the caller.
    #[allow(clippy::too_many_arguments)]
    fn enter_callable_body(
        &mut self,
        context: &Site,
        mut fun: Site,
        args: &mut Vec<Site>,
        params: &[Spanned<CPattern>],
        body: &Spanned<CExpr>,
        module: &[String],
        ambient: &str,
    ) -> Result<Site, EditError> {
        self.type_callable(context, &mut fun, args, ambient)?;
        let mut output = fun.ty.clone().unwrap();
        let scope = self.scopes.len();
        self.scopes.push(self.scopes[fun.scope].clone());
        for (param, arg) in params.iter().zip(args.drain(..params.len())) {
            self.bind_pattern(&param.node, arg, scope, module, false)?;
            let Ty::Function(_, rest) = output else {
                return Err(EditError::Internal("returned callable prefix type".into()));
            };
            output = *rest;
        }
        Ok(Site {
            module: module.to_vec(),
            scope,
            ty: Some(output),
            ..fun.child(body)
        })
    }
    /// Keep the original aggregate's checked callable/argument specialization
    /// before following its source fields. A Bool/String result alone must not
    /// let an Int capture silently default to Float after source reconstruction.
    fn type_callable(
        &mut self,
        context: &Site,
        fun: &mut Site,
        args: &mut [Site],
        ambient: &str,
    ) -> Result<(), EditError> {
        let (function, mut declarations) =
            self.render_site_typed(fun, "twineProjectedCallableType")?;
        let mut names = vec![];
        for (i, arg) in args.iter().enumerate() {
            let (value, helpers) =
                self.render_site_typed(arg, &format!("twineProjectedArg{i}Type"))?;
            declarations.push_str(&helpers);
            declarations.push_str(&format!("provided{i} =\n{}", indent(&value)));
            names.push(format!("provided{i}"));
        }
        if let Some(output) = &context.ty {
            declarations.push_str(&format!(
                "hold :: {} -> {}\nhold x = x\n",
                output.source(),
                output.source()
            ));
        } else {
            declarations.push_str("hold x = x\n");
        }
        let params = (0..args.len()).map(|i| format!("a{i}")).collect::<Vec<_>>();
        declarations.push_str(&format!(
            "callableFrame = (\\f {} -> (hold (f {}), f, {})) ({function}) {}\n",
            params.join(" "),
            params.join(" "),
            group(&params),
            names.join(" ")
        ));
        self.probes.declarations = ambient.into();
        let checked = self.probes.check(&declarations)?;
        let scheme = checked
            .schemes
            .get(&knot_checker::solve::SchemeKey::TopLevel(
                "callableFrame".into(),
            ))
            .unwrap();
        let Ty::Tuple(mut frame) = Ty::read(&mut checked.substitution.clone(), scheme.ty)? else {
            return Err(EditError::Internal("callable source frame".into()));
        };
        let inputs = frame.pop().unwrap();
        fun.ty = frame.pop();
        let inputs = if args.len() == 1 {
            vec![inputs]
        } else if let Ty::Tuple(inputs) = inputs {
            inputs
        } else {
            return Err(EditError::Internal("callable source arguments".into()));
        };
        for (arg, ty) in args.iter_mut().zip(inputs) {
            arg.ty = Some(ty);
        }
        Ok(())
    }
    pub(super) fn method_definition(
        &self,
        query: &TypedProgram,
    ) -> Result<(GlobalName, CFnDef), EditError> {
        // The checked frame is `(\fun args -> ...) selectedFunction args`.
        // Its first argument is the exact callable whose annotation we need;
        // nested argument expressions may use the same method spelling with a
        // different dictionary. Never scan unrelated references by name.
        let mut frame = &query
            .definition("frame")
            .ok_or_else(|| EditError::Internal("missing checked call frame".into()))?
            .body;
        let mut callable = None;
        while let TExpr::App(function, argument) = &frame.node {
            callable = Some(argument.as_ref());
            frame = function;
        }
        let callable =
            callable.ok_or_else(|| EditError::Internal("invalid checked call frame".into()))?;
        let site = EvidenceSite::Expr(callable.id);
        let Some(ResolvedReference::Method {
            name: method,
            dictionary_index,
            ..
        }) = query.references.get(&site)
        else {
            return unsupported("selected callable has no method evidence");
        };
        let Some(DictExpr::Instance {
            interface, head, ..
        }) = query
            .dictionaries
            .get(&site)
            .and_then(|ds| ds.get(*dictionary_index))
        else {
            return unsupported("selected callable needs a concrete instance dictionary");
        };
        for program in self
            .snapshot
            .graph
            .modules
            .iter()
            .map(|m| m.program.as_ref())
            .chain(self.bundled.iter())
        {
            if let Some(instance) = program
                .instances
                .iter()
                .find(|i| &i.interface == interface && &i.head == head)
            {
                if let CDecl::Instance(decl) = &program.canonical.decls[instance.declaration].node {
                    if let Some(f) = decl.methods.iter().find(|f| f.name == *method) {
                        return Ok((
                            GlobalName {
                                module: program.canonical.name.clone(),
                                name: method.clone(),
                            },
                            f.clone(),
                        ));
                    }
                }
            }
        }
        unsupported(&format!(
            "no reverse rule on the selected method dictionary for {}",
            method
        ))
    }
    pub(super) fn freeze(&mut self, s: Site) -> Result<Site, EditError> {
        let mut s = self.resolve_local(s)?;
        let scope = self.scopes.len();
        let captures = self.scopes[s.scope].clone();
        let mut frozen = BTreeMap::new();
        for (name, capture) in captures {
            let mut capture = self.resolve_local(capture)?;
            capture.locked = true;
            capture.fixed = true;
            frozen.insert(name, capture);
        }
        self.scopes.push(frozen);
        s.scope = scope;
        s.locked = true;
        Ok(s)
    }
}
