//! Queries are checked in the selected expression's lexical scope before the
//! session starts. They are lazy local bindings, available in every activation
//! that reaches the selection. No type-erased handle is cast to a guessed type.
use knot_checker::{
    check::check_program_with_imports,
    host::HostDeclarations,
    print::print_scheme,
    program::{NodeRef, TypedProgram},
};
use knot_syntax::{
    ast::{
        decl::{Decl, Module},
        expr::{DoStmt, Expr},
        pattern::Pattern,
    },
    span::{Span, Spanned},
};

pub struct PreparedQuery {
    pub program: TypedProgram,
    pub query_node: NodeRef,
    /// Original source followed by the separate query source for diagnostics.
    pub diagnostic_source: String,
}
/// Prepare one arbitrary pure expression. `value` refers to the selected value;
/// other names resolve in the selection's original lexical scope. The selection
/// must be an exact parser expression span. Adding a query requires recompiling
/// this revision and selecting its new activations; live query attachment to an
/// already-running revision is deliberately not a type-erased shortcut.
pub fn prepare(
    source: &str,
    target: Span,
    expression: &str,
    hosts: &HostDeclarations,
) -> Result<PreparedQuery, String> {
    prepare_with_imports(source, target, expression, hosts, hosts, &[])
}

/// Module-aware preparation against the same interface snapshot as the program.
/// Prefer `ModuleGraph::prepare_query` when using the filesystem loader.
pub fn prepare_with_imports(
    source: &str,
    target: Span,
    expression: &str,
    hosts: &HostDeclarations,
    registry: &dyn knot_canonical::ModuleRegistry,
    imports: &[&knot_checker::modules::CheckedInterface],
) -> Result<PreparedQuery, String> {
    let check = |ast: &Module| {
        let canonical = knot_canonical::canonicalize_module(ast, Some(registry))
            .map_err(|e| format!("{e:?}"))?;
        check_program_with_imports(&canonical, hosts, imports).map_err(|e| format!("{e:?}"))
    };
    let mut ast = knot_syntax::parse(source).map_err(|e| e.render(source))?;
    let mut original = check(&ast)?;
    let mut counter = 0;
    let (selected, query_name) = loop {
        let names = (
            format!("twineSelected{counter}"),
            format!("twineProbe{counter}"),
        );
        if !source.contains(&names.0)
            && !source.contains(&names.1)
            && !expression.contains(&names.0)
            && !expression.contains(&names.1)
        {
            break names;
        }
        counter += 1;
    };
    let query_source = format!(
        "module Query exposing (..)\nprobe =\n{}\n",
        expression
            .lines()
            .map(|l| format!("  {l}"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    let parsed = knot_syntax::parse(&query_source).map_err(|e| e.render(&query_source))?;
    if parsed.decls.len() != 1 {
        return Err("expected exactly one query expression".into());
    }
    // Parse again with an equal-length newline prefix to relocate all nested
    // expression, pattern, operator and annotation spans, not just RHS spans.
    let padded = format!("{}{query_source}", "\n".repeat(source.len() + 1));
    let mut parsed = knot_syntax::parse(&padded).map_err(|e| e.render(&padded))?;
    let Decl::Fn(f) = &mut parsed.decls[0].node else {
        return Err("expected query expression".into());
    };
    let query = f.body.clone();
    let query_span = query.span;
    let mut found = false;
    for decl in &mut ast.decls {
        let methods: Vec<_> = match &mut decl.node {
            Decl::Fn(f) => vec![f],
            Decl::Instance(i) => i.methods.iter_mut().collect(),
            _ => vec![],
        };
        for f in methods {
            visit(&mut f.body, &mut |e| {
                if e.span != target {
                    return false;
                }
                found = true;
                let old = e.clone();
                let selected_ref = Spanned::new(target, Expr::Var(selected.clone()));
                let query_body = Spanned::new(
                    query_span,
                    Expr::Let(
                        vec![(
                            Spanned::new(query_span, Pattern::Var("value".into())),
                            selected_ref.clone(),
                        )],
                        Box::new(query.clone()),
                    ),
                );
                e.node = Expr::Let(
                    vec![
                        (Spanned::new(target, Pattern::Var(selected.clone())), old),
                        (
                            Spanned::new(query_span, Pattern::Var(query_name.clone())),
                            query_body,
                        ),
                    ],
                    Box::new(selected_ref),
                );
                true
            });
        }
    }
    if !found {
        return Err("selection must be an exact expression span".into());
    }
    let mut program = check(&ast)?;
    // A preview must not specialize the original program or introduce new
    // interface requirements. Compare every original binding's inferred scheme.
    for binding in original.bindings.values() {
        let before = print_scheme(&mut original.substitution, &binding.scheme);
        let after = program
            .bindings
            .values()
            .find(|b| b.name == binding.name && b.span == binding.span)
            .ok_or_else(|| format!("query lost binding {}", binding.name))?;
        if before != print_scheme(&mut program.substitution, &after.scheme) {
            return Err(format!("query changes the type or constraints of {}; specialize the selected expression first", binding.name));
        }
    }
    let binding = program
        .bindings
        .values()
        .find(|b| b.name == query_name)
        .ok_or("missing query binding")?;
    if !binding.parameters.is_empty() {
        return Err("query needs unresolved dictionaries; specialize its result".into());
    }
    let query_node = find_query_node(&program, &query_name).ok_or("missing query RHS")?;
    Ok(PreparedQuery {
        program,
        query_node,
        diagnostic_source: format!("{source}\n{query_source}"),
    })
}
fn find_query_node(program: &TypedProgram, name: &str) -> Option<NodeRef> {
    // Core's named local binder points to the exact checked RHS and includes any
    // dictionary wrappers. This lookup is independent of duplicated source spans.
    let core = knot_core::lower(std::slice::from_ref(program));
    for expr in &core.expressions {
        if let knot_core::Kind::Let(bindings, _) = &expr.kind {
            for (local, rhs) in bindings {
                if core.local_names.get(local).is_some_and(|n| n == name) {
                    return Some(core.expressions[rhs.0].origin.node);
                }
            }
        }
    }
    None
}
fn visit(e: &mut Spanned<Expr>, f: &mut impl FnMut(&mut Spanned<Expr>) -> bool) {
    if f(e) {
        return;
    }
    match &mut e.node {
        Expr::Lambda(_, b) | Expr::Negate(b) | Expr::FieldAccess(b, _) | Expr::Annotated(_, b) => {
            visit(b, f)
        }
        Expr::App(a, b) => {
            visit(a, f);
            visit(b, f);
        }
        Expr::OpChain(a, bs) => {
            visit(a, f);
            for (_, b) in bs {
                visit(b, f);
            }
        }
        Expr::If(a, b, c) => {
            visit(a, f);
            visit(b, f);
            visit(c, f);
        }
        Expr::Let(bs, b) | Expr::Case(b, bs) => {
            for (_, e) in bs {
                visit(e, f);
            }
            visit(b, f);
        }
        Expr::Do(stmts, b) => {
            for s in stmts {
                match s {
                    DoStmt::Bind(_, e) | DoStmt::Expr(e) => visit(e, f),
                    DoStmt::Let(bs) => {
                        for (_, e) in bs {
                            visit(e, f);
                        }
                    }
                }
            }
            visit(b, f);
        }
        Expr::List(es) | Expr::Tuple(es) => {
            for e in es {
                visit(e, f);
            }
        }
        Expr::Record(fs) => {
            for (_, e) in fs {
                visit(e, f);
            }
        }
        Expr::RecordUpdate(b, fs) => {
            visit(b, f);
            for (_, e) in fs {
                visit(e, f);
            }
        }
        _ => {}
    }
}
