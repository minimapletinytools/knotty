//! Conservative source immutability for explicit callback capabilities. Pinning
//! syntax/dependencies avoids requiring equality or materialization of closures.
use super::*;
use knot_canonical::ast::CDoStmt;
use std::collections::BTreeSet;

#[derive(Default)]
pub(super) struct SourcePins {
    spans: BTreeSet<(u32, u32)>,
    globals: BTreeSet<GlobalName>,
    instances: BTreeSet<(String, GlobalName)>,
    code_nodes: usize,
    nodes: usize,
}
impl SourcePins {
    pub(super) fn restart_attempt(&mut self) {
        self.spans.clear();
        self.globals.clear();
        self.instances.clear();
        // Source-dependency work remains cumulative across attempts.
    }
}
impl Walker<'_> {
    pub(super) fn validate_source_pins(&self) -> Result<(), EditError> {
        if self.changed_spans.iter().any(|(start, end)| {
            self.source_pins.spans.iter().any(|(a, b)| {
                if start == end {
                    a <= start && start < b
                } else {
                    start < b && a < end
                }
            })
        }) {
            return Err(EditError::ValidationFailed);
        }
        Ok(())
    }
    pub(super) fn pin_source_dependencies(&mut self, root: Site) -> Result<(), EditError> {
        let entry = self.snapshot.graph.entry().program.canonical.name.clone();
        let ty = root.ty.as_ref().ok_or_else(|| {
            EditError::Internal("callback dependency needs a concrete type witness".into())
        })?;
        let (source, helpers) = syntax::render_typed(&root, &self.scopes, "twineDependencyType")?;
        let ty = ty.clone();
        self.pin_syntax_dependencies(vec![(root, BTreeSet::new())])?;
        let dependencies = self.probes.dependency_globals(
            &source,
            &ty,
            &helpers,
            &mut self.source_pins.code_nodes,
        )?;
        let mut pending = vec![];
        let program = &self.snapshot.graph.entry().program;
        for dependency in dependencies {
            let mut definitions = vec![];
            match dependency {
                knot_core::Global::Instance(interface, head) => {
                    if !self
                        .source_pins
                        .instances
                        .insert((interface.clone(), head.clone()))
                    {
                        continue;
                    }
                    for instance in program
                        .instances
                        .iter()
                        .filter(|i| i.interface == interface && i.head == head)
                    {
                        if let CDecl::Instance(instance) =
                            &program.canonical.decls[instance.declaration].node
                        {
                            definitions.extend(instance.methods.iter().cloned());
                        }
                    }
                }
                knot_core::Global::Value(name) if name.module == entry => {
                    if self.source_pins.globals.insert(name.clone()) {
                        if let Some(f) = self.definition(&name) {
                            definitions.push(f);
                        }
                    }
                }
                knot_core::Global::Default(interface, method) => {
                    for default in program.defaults.iter().filter(|d| {
                        d.interface == interface
                            && program.bindings[&d.definition.binding].name == method
                    }) {
                        if let CDecl::Interface(interface) =
                            &program.canonical.decls[default.declaration].node
                        {
                            definitions.extend(
                                interface
                                    .defaults
                                    .iter()
                                    .filter(|f| f.name == method)
                                    .cloned(),
                            );
                        }
                    }
                }
                _ => {}
            }
            for definition in definitions {
                let mut bound = BTreeSet::new();
                for p in &definition.params {
                    syntax::bind_names(&p.node, &mut bound);
                }
                pending.push((
                    Site {
                        recipe: None,
                        candidate_source: false,
                        via: None,
                        lash: None,
                        cleat: None,
                        synthetic: false,
                        module: entry.clone(),
                        expr: definition.body,
                        scope: 0,
                        projections: vec![],
                        locked: true,
                        fixed: true,
                        ty: None,
                    },
                    bound,
                ));
            }
        }
        self.pin_syntax_dependencies(pending)
    }
    fn pin_syntax_dependencies(
        &mut self,
        mut pending: Vec<(Site, BTreeSet<String>)>,
    ) -> Result<(), EditError> {
        let entry = self.snapshot.graph.entry().program.canonical.name.clone();
        while let Some((site, bound)) = pending.pop() {
            self.source_pins.nodes += 1;
            if self.source_pins.nodes > 4096 {
                return unsupported("callback source dependencies exceeded 4096 nodes");
            }
            if site.module == entry {
                self.source_pins
                    .spans
                    .insert((site.expr.span.start, site.expr.span.end));
            }
            let mut reference = |r: &Ref| -> Result<(), EditError> {
                if let Ref::Local(name) = r {
                    if !bound.contains(name) {
                        let capture =
                            self.scopes[site.scope].get(name).cloned().ok_or_else(|| {
                                EditError::Unsupported(format!("missing callback capture {name}"))
                            })?;
                        pending.push((capture, BTreeSet::new()));
                    }
                    return Ok(());
                }
                let name = syntax::reference(r, &site.module);
                if name.module == ["TwineTraversalQuery"] {
                    return unsupported(
                        "callback factories cannot refer to generated query bindings",
                    );
                }
                if name.module == entry && self.source_pins.globals.insert(name.clone()) {
                    if let Some(f) = self.definition(&name) {
                        let mut bound = BTreeSet::new();
                        for p in &f.params {
                            syntax::bind_names(&p.node, &mut bound);
                        }
                        pending.push((
                            Site {
                                synthetic: false,
                                module: entry.clone(),
                                expr: f.body,
                                scope: 0,
                                ..site.clone()
                            },
                            bound,
                        ));
                    }
                }
                Ok(())
            };
            match &site.expr.node {
                CExpr::Var(r) | CExpr::Ctor(r) => reference(r)?,
                CExpr::BinOp(r, a, b) => {
                    reference(r)?;
                    pending.push((site.child(a), bound.clone()));
                    pending.push((site.child(b), bound));
                }
                CExpr::App(a, b) => {
                    pending.push((site.child(a), bound.clone()));
                    pending.push((site.child(b), bound));
                }
                CExpr::Negate(e) => {
                    reference(&Ref::Builtin("negate".into()))?;
                    pending.push((site.child(e), bound));
                }
                CExpr::Annotated(_, e) | CExpr::FieldAccess(e, _) => {
                    pending.push((site.child(e), bound))
                }
                CExpr::If(c, t, f) => {
                    for e in [c, t, f] {
                        pending.push((site.child(e), bound.clone()));
                    }
                }
                CExpr::Lambda(params, body) => {
                    let mut bound = bound;
                    for p in params {
                        syntax::bind_names(&p.node, &mut bound);
                    }
                    pending.push((site.child(body), bound));
                }
                CExpr::Let(bindings, body) => {
                    let mut bound = bound;
                    for (p, _) in bindings {
                        syntax::bind_names(&p.node, &mut bound);
                    }
                    for (_, e) in bindings {
                        pending.push((site.child(e), bound.clone()));
                    }
                    pending.push((site.child(body), bound));
                }
                CExpr::Case(value, arms) => {
                    pending.push((site.child(value), bound.clone()));
                    for (p, e) in arms {
                        let mut bound = bound.clone();
                        syntax::bind_names(&p.node, &mut bound);
                        pending.push((site.child(e), bound));
                    }
                }
                CExpr::Do(stmts, body) => {
                    let mut bound = bound;
                    for stmt in stmts {
                        match stmt {
                            CDoStmt::Let(bindings) => {
                                for (p, _) in bindings {
                                    syntax::bind_names(&p.node, &mut bound);
                                }
                                for (_, e) in bindings {
                                    pending.push((site.child(e), bound.clone()));
                                }
                            }
                            CDoStmt::Bind(p, e) => {
                                pending.push((site.child(e), bound.clone()));
                                syntax::bind_names(&p.node, &mut bound);
                            }
                            CDoStmt::Expr(e) => pending.push((site.child(e), bound.clone())),
                        }
                    }
                    pending.push((site.child(body), bound));
                }
                CExpr::List(es) | CExpr::Tuple(es) => {
                    for e in es {
                        pending.push((site.child(e), bound.clone()));
                    }
                }
                CExpr::Record(fs) => {
                    for (_, e) in fs {
                        pending.push((site.child(e), bound.clone()));
                    }
                }
                CExpr::RecordUpdate(base, fs) => {
                    pending.push((site.child(base), bound.clone()));
                    for (_, e) in fs {
                        pending.push((site.child(e), bound.clone()));
                    }
                }
                CExpr::IntLit(_) => reference(&Ref::Builtin("fromInteger".into()))?,
                CExpr::FloatLit(_) => reference(&Ref::Builtin("fromRational".into()))?,
                CExpr::Unit | CExpr::StringLit(_) | CExpr::Hole => {}
            }
        }
        Ok(())
    }
}
