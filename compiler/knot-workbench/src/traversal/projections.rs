//! Source projections carry original expressions/spans, never synthetic patch
//! targets. Matching and branch guards run as ordinary checked Knot queries.
use super::*;

pub(super) enum Focus {
    Direct(Site),
    Computed(Site),
}

#[derive(Clone)]
pub(super) enum ProjectionKind {
    Field(String),
    Tuple {
        arity: usize,
        index: usize,
    },
    Constructor {
        name: GlobalName,
        arity: usize,
        index: usize,
    },
    Match {
        pattern: CPattern,
        module: Vec<String>,
    },
}
#[derive(Clone)]
pub(super) struct Projection {
    pub kind: ProjectionKind,
    pub input: Option<Ty>,
}
impl Site {
    pub fn projected(&self, kind: ProjectionKind, output: Option<Ty>) -> Self {
        let mut next = self.clone();
        next.projections.push(Projection {
            kind,
            input: self.ty.clone(),
        });
        next.ty = output;
        next
    }
}
/// The same projection expression is used against original and candidate source.
pub(super) fn render(
    mut value: String,
    projections: &[Projection],
    typing: Option<&syntax::Typing>,
) -> Result<String, EditError> {
    for projection in projections {
        if let (Some(typing), Some(ty)) = (typing, &projection.input) {
            value = typing.wrap(ty, value);
        }
        if let ProjectionKind::Field(field) = &projection.kind {
            value = format!("({value}).{field}");
            continue;
        }
        let pattern_text = match &projection.kind {
            ProjectionKind::Match { pattern, module } => syntax::pattern(pattern, module),
            _ => String::new(),
        };
        let mut index = 0;
        let name = loop {
            let name = format!("twineProjection{index}");
            if !value.contains(&name) && !pattern_text.contains(&name) {
                break name;
            }
            index += 1;
        };
        let slots = |arity: usize, selected: usize| {
            (0..arity)
                .map(|i| {
                    if i == selected {
                        name.clone()
                    } else {
                        "_".into()
                    }
                })
                .collect::<Vec<_>>()
        };
        let pattern = match &projection.kind {
            ProjectionKind::Tuple { arity, index } => group(&slots(*arity, *index)),
            ProjectionKind::Constructor {
                name: ctor,
                arity,
                index,
            } => format!(
                "({})",
                constructor_rules::constructor_pattern(ctor, &slots(*arity, *index))
            ),
            ProjectionKind::Match { .. } => format!("({pattern_text} as {name})"),
            ProjectionKind::Field(_) => unreachable!(),
        };
        value = format!("(case {value} of\n  {pattern} -> {name}\n  _ -> error \"source projection pattern failed\")");
    }
    Ok(value)
}

impl Walker<'_> {
    pub(super) fn visit_site(&mut self) -> Result<(), EditError> {
        self.visits += 1;
        if self.visits > self.probes.limits.traversal.source_sites {
            return unsupported("source traversal exceeded its site budget");
        }
        Ok(())
    }
    fn retain_match(
        &mut self,
        value: &Site,
        pattern: &CPattern,
        module: &[String],
    ) -> Result<(), EditError> {
        let predicate = format!(
            "case current of\n  {} -> True\n  _ -> False",
            syntax::pattern(pattern, module)
        );
        self.probes.declarations = self.current_declaration(value)?;
        if !self.probes.boolean(&predicate)? {
            return unsupported("source pattern no longer matches");
        }
        self.guards.push((value.clone(), predicate));
        Ok(())
    }
    pub(super) fn bind_pattern(
        &mut self,
        pattern: &CPattern,
        value: Site,
        scope: usize,
        module: &[String],
        lazy: bool,
    ) -> Result<(), EditError> {
        if let CPattern::Var(name) = pattern {
            self.scopes[scope].insert(name.clone(), value);
            return Ok(());
        }
        let value = if lazy {
            value.projected(
                ProjectionKind::Match {
                    pattern: pattern.clone(),
                    module: module.to_vec(),
                },
                value.ty.clone(),
            )
        } else {
            self.retain_match(&value, pattern, module)?;
            value
        };
        self.bind_parts(pattern, value, scope, module);
        Ok(())
    }
    fn bind_parts(&mut self, pattern: &CPattern, value: Site, scope: usize, module: &[String]) {
        match pattern {
            CPattern::Var(name) => {
                self.scopes[scope].insert(name.clone(), value);
            }
            CPattern::As(p, name) => {
                self.scopes[scope].insert(name.clone(), value.clone());
                self.bind_parts(&p.node, value, scope, module);
            }
            CPattern::Tuple(parts) => {
                for (index, p) in parts.iter().enumerate() {
                    let ty = match &value.ty {
                        Some(Ty::Tuple(ts)) => ts.get(index).cloned(),
                        _ => None,
                    };
                    self.bind_parts(
                        &p.node,
                        value.projected(
                            ProjectionKind::Tuple {
                                arity: parts.len(),
                                index,
                            },
                            ty,
                        ),
                        scope,
                        module,
                    );
                }
            }
            CPattern::Record(fields) => {
                for name in fields {
                    let ty = match &value.ty {
                        Some(Ty::Record(ts)) => ts.get(name).cloned(),
                        _ => None,
                    };
                    self.scopes[scope].insert(
                        name.clone(),
                        value.projected(ProjectionKind::Field(name.clone()), ty),
                    );
                }
            }
            CPattern::Ctor(ctor, parts) => {
                for (index, p) in parts.iter().enumerate() {
                    self.bind_parts(
                        &p.node,
                        value.projected(
                            ProjectionKind::Constructor {
                                name: reference(ctor, module),
                                arity: parts.len(),
                                index,
                            },
                            None,
                        ),
                        scope,
                        module,
                    );
                }
            }
            CPattern::Cons(head, tail) => {
                for (index, p) in [head, tail].into_iter().enumerate() {
                    self.bind_parts(
                        &p.node,
                        value.projected(
                            ProjectionKind::Constructor {
                                name: GlobalName {
                                    module: vec!["Prelude".into()],
                                    name: ":".into(),
                                },
                                arity: 2,
                                index,
                            },
                            None,
                        ),
                        scope,
                        module,
                    );
                }
            }
            _ => {}
        }
    }
    pub(super) fn let_site(
        &mut self,
        parent: &Site,
        bindings: &[(Spanned<CPattern>, Spanned<CExpr>)],
        body: &Spanned<CExpr>,
    ) -> Result<Site, EditError> {
        let scope = self.scopes.len();
        self.scopes.push(self.scopes[parent.scope].clone());
        for (pattern, value) in bindings {
            self.bind_pattern(
                &pattern.node,
                Site {
                    scope,
                    ..parent.child(value)
                },
                scope,
                &parent.module,
                true,
            )?;
        }
        Ok(Site {
            scope,
            ty: parent.ty.clone(),
            ..parent.child(body)
        })
    }
    pub(super) fn case_site(
        &mut self,
        parent: &Site,
        value: &Spanned<CExpr>,
        arms: &[(Spanned<CPattern>, Spanned<CExpr>)],
    ) -> Result<Site, EditError> {
        let value = self.resolve_local(parent.child(value))?;
        let arms_source = arms
            .iter()
            .enumerate()
            .map(|(i, (p, _))| format!("  {} -> {i}", syntax::pattern(&p.node, &parent.module)))
            .collect::<Vec<_>>()
            .join("\n");
        let choice = format!("(case current of\n{arms_source})");
        self.probes.declarations = self.current_declaration(&value)?;
        let (selected, _) = self
            .probes
            .evaluate(&choice, Some(&Ty::Named("Int".into(), vec![])))?;
        let Data::Literal(knot_core::Literal::Int(index)) = selected else {
            return Err(EditError::Internal("case branch index".into()));
        };
        let (pattern, body) = arms
            .get(index as usize)
            .ok_or_else(|| EditError::Internal("case branch out of bounds".into()))?;
        // Retain the whole ordered match: merely keeping the chosen pattern
        // true would miss an edit that starts matching an earlier alternative.
        self.guards
            .push((value.clone(), format!("{choice} == {index}")));
        let scope = self.scopes.len();
        self.scopes.push(self.scopes[parent.scope].clone());
        self.bind_parts(&pattern.node, value, scope, &parent.module);
        Ok(Site {
            scope,
            ty: parent.ty.clone(),
            ..parent.child(body)
        })
    }
    pub(super) fn focus_projection(&mut self, site: Site) -> Result<Focus, EditError> {
        self.focus_projection_mode(site, false)
    }
    pub(super) fn focus_callable(&mut self, site: Site) -> Result<Site, EditError> {
        match self.focus_projection_mode(site, true)? {
            Focus::Direct(site) => Ok(site),
            Focus::Computed(_) => unsupported("computed callable needs a source companion"),
        }
    }
    fn focus_projection_mode(
        &mut self,
        mut site: Site,
        read_only: bool,
    ) -> Result<Focus, EditError> {
        let first = site.projections.remove(0);
        let rest = std::mem::take(&mut site.projections);
        let output = site.ty.take();
        site.ty = first.input;
        let mut focused = self.focus_step(site, &first.kind, read_only)?;
        let found = match &mut focused {
            Focus::Direct(s) | Focus::Computed(s) => s,
        };
        found.projections.extend(rest);
        found.ty = output;
        Ok(focused)
    }
    fn focus_step(
        &mut self,
        mut site: Site,
        projection: &ProjectionKind,
        read_only: bool,
    ) -> Result<Focus, EditError> {
        if let ProjectionKind::Match { pattern, module } = projection {
            self.retain_match(&site, pattern, module)?;
            return Ok(Focus::Direct(site));
        }
        loop {
            self.visit_site()?;
            site = self.resolve_local(site)?;
            if site.fixed && !read_only {
                return unsupported("captured callback value is pinned");
            }
            if !read_only && self.solver_pinned(&site) {
                return Err(EditError::ValidationFailed);
            }
            if !site.projections.is_empty() {
                match self.focus_projection_mode(site, read_only)? {
                    Focus::Direct(found) => site = found,
                    Focus::Computed(found) => {
                        return Ok(Focus::Computed(found.projected(projection.clone(), None)))
                    }
                }
                continue;
            }
            if let Some(mut next) = direct(&site, projection) {
                if read_only {
                    next.fixed |= site.fixed;
                }
                return Ok(Focus::Direct(next));
            }
            site = match &site.expr.node {
                CExpr::Annotated(_, e) => Site {
                    ty: site.ty.clone(),
                    ..site.child(e)
                },
                CExpr::Var(r) => {
                    let name = reference(r, &site.module);
                    if !read_only
                        && name.module != self.snapshot.graph.entry().program.canonical.name
                    {
                        return Ok(Focus::Computed(site.projected(projection.clone(), None)));
                    }
                    let f = self
                        .definition(&name)
                        .ok_or_else(|| EditError::Unsupported("opaque projected source".into()))?;
                    if !f.params.is_empty() || f.annotations.iter().any(|a| a.key == "unravel") {
                        return Ok(Focus::Computed(site.projected(projection.clone(), None)));
                    }
                    Site {
                        module: name.module,
                        expr: f.body,
                        scope: 0,
                        ..site
                    }
                }
                CExpr::If(c, yes, no) => {
                    let taken = self.condition(&site.child(c))?;
                    Site {
                        ty: site.ty.clone(),
                        ..site.child(if taken { yes } else { no })
                    }
                }
                CExpr::FieldAccess(base, name) => site
                    .child(base)
                    .projected(ProjectionKind::Field(name.clone()), site.ty.clone()),
                CExpr::Let(bindings, body) => self.let_site(&site, bindings, body)?,
                CExpr::Case(value, arms) => self.case_site(&site, value, arms)?,
                _ => return Ok(Focus::Computed(site.projected(projection.clone(), None))),
            };
        }
    }
    pub(super) fn projected_pin(&self, site: &Site, depth: usize) -> bool {
        if site.fixed || depth > 128 {
            return true;
        }
        let mut base = site.clone();
        let projections = std::mem::take(&mut base.projections);
        for projection in projections {
            if matches!(projection.kind, ProjectionKind::Match { .. }) {
                continue;
            }
            // Pin classification must not evaluate unused arguments. Follow
            // static fields when possible; otherwise use the aggregate's fixed
            // source capability. The checked update still validates siblings.
            for _ in 0..128 {
                if base.fixed || self.solver_pinned(&base) {
                    return true;
                }
                if !base.projections.is_empty() {
                    return true;
                }
                if let CExpr::Var(Ref::Local(n)) = &base.expr.node {
                    let Some(next) = self.scopes[base.scope].get(n) else {
                        return true;
                    };
                    base = next.clone();
                    continue;
                }
                match &base.expr.node {
                    CExpr::Annotated(_, expr) => {
                        base = base.child(expr);
                        continue;
                    }
                    CExpr::Var(r) => {
                        let name = reference(r, &base.module);
                        if name.module != self.snapshot.graph.entry().program.canonical.name {
                            break;
                        }
                        let Some(f) = self.definition(&name) else {
                            return true;
                        };
                        if !f.params.is_empty() || f.annotations.iter().any(|a| a.key == "unravel")
                        {
                            break;
                        }
                        base = Site {
                            module: name.module,
                            expr: f.body,
                            scope: 0,
                            ..base
                        };
                        continue;
                    }
                    _ => break,
                }
            }
            let Some(next) = direct(&base, &projection.kind) else {
                return self.is_pinned(&base, depth + 1);
            };
            base = next;
        }
        self.is_pinned(&base, depth + 1)
    }
}
/// Structural source metadata only; no constraint values or library policies.
pub(super) fn direct(site: &Site, projection: &ProjectionKind) -> Option<Site> {
    match (&site.expr.node, projection) {
        (CExpr::Record(fields), ProjectionKind::Field(name)) => fields
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, e)| site.child(e)),
        (CExpr::RecordUpdate(base, fields), ProjectionKind::Field(name)) => Some(
            fields
                .iter()
                .find(|(n, _)| n == name)
                .map(|(_, e)| site.child(e))
                .unwrap_or_else(|| site.child(base).projected(projection.clone(), None)),
        ),
        (CExpr::Tuple(fields), ProjectionKind::Tuple { arity, index })
            if fields.len() == *arity =>
        {
            fields.get(*index).map(|e| site.child(e))
        }
        (
            CExpr::List(_),
            ProjectionKind::Constructor {
                name,
                arity: 2,
                index,
            },
        ) if name.module == ["Prelude"] && name.name == ":" => site
            .list_fields()
            .and_then(|fields| fields.get(*index).cloned()),
        (
            CExpr::App(_, _) | CExpr::BinOp(_, _, _),
            ProjectionKind::Constructor { name, arity, index },
        ) => {
            let (fun, args) = flatten(site);
            match &fun.expr.node {
                CExpr::Ctor(r) | CExpr::Var(r)
                    if reference(r, &fun.module) == *name && args.len() == *arity =>
                {
                    args.get(*index).cloned()
                }
                _ => None,
            }
        }
        _ => None,
    }
}

impl Site {
    /// Syntax-only list desugaring. The tail shell is synthetic; original
    /// element sites remain writable, while replacing the shell needs a recipe.
    pub(super) fn list_fields(&self) -> Option<[Site; 2]> {
        let CExpr::List(elements) = &self.expr.node else {
            return None;
        };
        let (head, rest) = elements.split_first()?;
        let mut head = self.child(head);
        if let Some(Ty::Named(name, types)) = &self.ty {
            if name == "List" {
                head.ty = types.first().cloned();
            }
        }
        let tail = Site {
            synthetic: true,
            expr: Spanned::new(self.expr.span, CExpr::List(rest.to_vec())),
            recipe: recipes::Recipe::field(
                self,
                GlobalName {
                    module: vec!["Prelude".into()],
                    name: ":".into(),
                },
                vec![head.clone()],
                vec![],
                knot_syntax::Span::new(
                    rest.first()
                        .map_or(self.expr.span.end - 1, |e| e.span.start),
                    self.expr.span.end - 1,
                ),
            ),
            ..self.clone()
        };
        Some([head, tail])
    }
}
