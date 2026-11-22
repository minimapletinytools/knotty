use super::*;
use knot_syntax::ast::pattern::PatternLiteral;
use std::{cell::RefCell, collections::BTreeSet};

#[derive(Clone)]
pub(super) struct Site {
    /// Synthesized aggregate shell with original child spans, never a patch target.
    pub synthetic: bool,
    /// Candidate AST spans belong to the checked new revision. Original lexical
    /// captures still use their own sites and original-to-candidate mappings.
    pub candidate_source: bool,
    pub recipe: Option<Arc<recipes::Recipe>>,
    pub projections: Vec<projections::Projection>,
    pub module: Vec<String>,
    pub expr: Spanned<CExpr>,
    pub scope: usize,
    pub locked: bool,
    /// The entire captured value is fixed, independently of its internal syntax.
    pub fixed: bool,
    pub ty: Option<Ty>,
}
impl Site {
    pub fn child(&self, expr: &Spanned<CExpr>) -> Self {
        Self {
            synthetic: false,
            candidate_source: self.candidate_source,
            recipe: None,
            projections: vec![],
            expr: expr.clone(),
            ty: None,
            fixed: false,
            module: self.module.clone(),
            scope: self.scope,
            locked: self.locked,
        }
    }
}
pub(super) fn reference(r: &Ref, module: &[String]) -> GlobalName {
    match r {
        Ref::Imported { module, name } => GlobalName {
            module: module.clone(),
            name: name.clone(),
        },
        Ref::Builtin(name) => GlobalName {
            module: vec!["Prelude".into()],
            name: name.clone(),
        },
        Ref::TopLevel(name) => GlobalName {
            module: module.to_vec(),
            name: name.clone(),
        },
        Ref::Local(name) | Ref::Unresolved(name) => GlobalName {
            module: vec![],
            name: name.clone(),
        },
    }
}
pub(super) type Scopes = Vec<BTreeMap<String, Site>>;
pub(super) fn render_current(
    site: &Site,
    scopes: &Scopes,
    patches: &BTreeMap<(u32, u32), Spanned<CExpr>>,
    entry: &[String],
) -> Result<String, EditError> {
    render_captured(site, scopes, Some((patches, entry)), None)
}
// Checked capture types must survive source reconstruction. In particular,
// a Bool-valued comparison must not default an Int capture to Float.
pub(super) struct Typing {
    prefix: String,
    identities: RefCell<BTreeMap<String, String>>,
}
impl Typing {
    pub fn wrap(&self, ty: &Ty, value: String) -> String {
        let mut identities = self.identities.borrow_mut();
        let next = format!("{}{}", self.prefix, identities.len());
        let name = identities.entry(ty.source()).or_insert(next);
        format!("({name} {value})")
    }
}
pub(super) fn render_typed(
    site: &Site,
    scopes: &Scopes,
    prefix: &str,
) -> Result<(String, String), EditError> {
    typed(site, scopes, None, prefix)
}
pub(super) fn render_patched_typed(
    site: &Site,
    scopes: &Scopes,
    patches: &BTreeMap<(u32, u32), Spanned<CExpr>>,
    entry: &[String],
    prefix: &str,
) -> Result<(String, String), EditError> {
    typed(site, scopes, Some((patches, entry)), prefix)
}
fn typed(
    site: &Site,
    scopes: &Scopes,
    patches: Replacements<'_>,
    prefix: &str,
) -> Result<(String, String), EditError> {
    let plain = render_captured(site, scopes, patches, None)?;
    let mut prefix = prefix.to_string();
    while plain.contains(&prefix) {
        prefix.push('x');
    }
    let typing = Typing {
        prefix,
        identities: RefCell::new(BTreeMap::new()),
    };
    let expression = render_captured(site, scopes, patches, Some(&typing))?;
    let declarations = typing
        .identities
        .into_inner()
        .into_iter()
        .map(|(ty, name)| format!("{name} :: {ty} -> {ty}\n{name} value = value\n"))
        .collect();
    Ok((expression, declarations))
}
type Replacements<'a> = Option<(&'a BTreeMap<(u32, u32), Spanned<CExpr>>, &'a [String])>;
mod captures;
use captures::{render_captured, Captures};

fn render_inner(
    s: &Site,
    scopes: &Scopes,
    bound: &BTreeSet<String>,
    depth: usize,
    patches: Replacements<'_>,
    typing: Option<&Typing>,
    captures: &RefCell<Captures>,
) -> Result<String, EditError> {
    if depth > 128 {
        return unsupported("cyclic or deeply nested source expression");
    }
    if let Some((values, _)) = patches.filter(|_| s.synthetic) {
        if let Some(recipe) = s.recipe.as_ref().filter(|recipe| {
            values.contains_key(&(recipe.root.expr.span.start, recipe.root.expr.span.end))
        }) {
            let root = render_inner(
                &recipe.root,
                scopes,
                bound,
                depth + 1,
                patches,
                typing,
                captures,
            )?;
            let projected = recipe.project(root, typing)?;
            return projections::render(projected, &s.projections, typing);
        }
    }
    if let Some((values, entry)) = patches {
        if s.module == entry && !s.synthetic && !s.candidate_source {
            if let Some(expression) = values.get(&(s.expr.span.start, s.expr.span.end)) {
                let mut candidate = s.clone();
                candidate.expr = expression.clone();
                candidate.candidate_source = true;
                return render_inner(
                    &candidate,
                    scopes,
                    bound,
                    depth + 1,
                    patches,
                    typing,
                    captures,
                );
            }
        }
    }
    let child = |e: &Spanned<CExpr>| {
        render_inner(
            &s.child(e),
            scopes,
            bound,
            depth + 1,
            patches,
            typing,
            captures,
        )
    };
    let named = |r: &Ref| -> Result<String, EditError> {
        if let Ref::Local(n) = r {
            if bound.contains(n) {
                return Ok(n.clone());
            }
            let capture = scopes[s.scope]
                .get(n)
                .ok_or_else(|| EditError::Unsupported(format!("unbound source capture {n}")))?;
            let value = captures.borrow_mut().reference(s.scope, n, capture)?;
            return Ok(match (typing, &capture.ty) {
                (Some(typing), Some(ty)) => typing.wrap(ty, value),
                _ => value,
            });
        }
        Ok(global_source(&reference(r, &s.module)))
    };
    let rendered = match &s.expr.node {
        CExpr::Var(r) | CExpr::Ctor(r) => named(r)?,
        CExpr::IntLit(n) => format!("({n})"),
        CExpr::FloatLit(n) => format!("({}e{})", n.significand(), n.exponent()),
        CExpr::StringLit(t) => quoted(t),
        CExpr::Unit => "()".into(),
        CExpr::App(f, a) => format!("({} {})", child(f)?, child(a)?),
        CExpr::BinOp(r, a, b) => format!("({} {} {})", named(r)?, child(a)?, child(b)?),
        CExpr::Negate(a) => format!("(Prelude.negate {})", child(a)?),
        CExpr::Annotated(_, e) => child(e)?,
        CExpr::Tuple(es) => format!(
            "({})",
            es.iter()
                .map(child)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        CExpr::List(es) => format!(
            "[{}]",
            es.iter()
                .map(child)
                .collect::<Result<Vec<_>, _>>()?
                .join(", ")
        ),
        CExpr::Record(fs) => format!(
            "{{ {} }}",
            fs.iter()
                .map(|(n, e)| Ok(format!("{n} = {}", child(e)?)))
                .collect::<Result<Vec<_>, EditError>>()?
                .join(", ")
        ),
        CExpr::RecordUpdate(b, fs) => {
            let base = child(b)?;
            let fields = fs
                .iter()
                .map(|(n, e)| Ok(format!("{n} = {}", child(e)?)))
                .collect::<Result<Vec<_>, EditError>>()?
                .join(", ");
            let mut n = 0;
            let name = loop {
                let name = format!("twineRecord{n}");
                if !base.contains(&name) && !fields.contains(&name) {
                    break name;
                }
                n += 1;
            };
            format!("((\\{name} -> {{ {name} | {fields} }}) {base})")
        }
        CExpr::FieldAccess(b, n) => format!("({}).{n}", child(b)?),
        CExpr::If(c, t, f) => format!("(if {} then {} else {})", child(c)?, child(t)?, child(f)?),
        CExpr::Lambda(ps, b) => {
            let mut locals = bound.clone();
            for p in ps {
                bind_names(&p.node, &mut locals);
            }
            format!(
                "(\\{} ->\n{})",
                ps.iter()
                    .map(|p| pattern(&p.node, &s.module))
                    .collect::<Vec<_>>()
                    .join(" "),
                indent(&render_inner(
                    &s.child(b),
                    scopes,
                    &locals,
                    depth + 1,
                    patches,
                    typing,
                    captures
                )?)
            )
        }
        CExpr::Let(bs, b) => {
            let mut locals = bound.clone();
            for (p, _) in bs {
                bind_names(&p.node, &mut locals);
            }
            let bindings = bs
                .iter()
                .map(|(p, e)| {
                    Ok(format!(
                        "{} =\n{}",
                        pattern(&p.node, &s.module),
                        indent(&render_inner(
                            &s.child(e),
                            scopes,
                            &locals,
                            depth + 1,
                            patches,
                            typing,
                            captures
                        )?)
                    ))
                })
                .collect::<Result<Vec<_>, EditError>>()?
                .join("\n");
            format!(
                "(let\n{}in\n{})",
                indent(&bindings),
                indent(&render_inner(
                    &s.child(b),
                    scopes,
                    &locals,
                    depth + 1,
                    patches,
                    typing,
                    captures
                )?)
            )
        }
        CExpr::Case(e, bs) => {
            let arms = bs
                .iter()
                .map(|(p, b)| {
                    let mut locals = bound.clone();
                    bind_names(&p.node, &mut locals);
                    Ok(format!(
                        "{} ->\n{}",
                        pattern(&p.node, &s.module),
                        indent(&render_inner(
                            &s.child(b),
                            scopes,
                            &locals,
                            depth + 1,
                            patches,
                            typing,
                            captures
                        )?)
                    ))
                })
                .collect::<Result<Vec<_>, EditError>>()?
                .join("\n");
            format!("(case {} of\n{})", child(e)?, indent(&arms))
        }
        _ => return unsupported("source traversal does not support do/holes"),
    };
    let rendered = projections::render(rendered, &s.projections, typing)?;
    if rendered.len() > 1024 * 1024 {
        return Err(EditError::SourceTooLarge);
    }
    Ok(rendered)
}
pub(super) fn bind_names(p: &CPattern, names: &mut BTreeSet<String>) {
    match p {
        CPattern::Var(n) => {
            names.insert(n.clone());
        }
        CPattern::As(p, n) => {
            names.insert(n.clone());
            bind_names(&p.node, names);
        }
        CPattern::Tuple(ps) | CPattern::Ctor(_, ps) => {
            for p in ps {
                bind_names(&p.node, names);
            }
        }
        CPattern::Cons(a, b) => {
            bind_names(&a.node, names);
            bind_names(&b.node, names);
        }
        CPattern::Record(ns) => names.extend(ns.iter().cloned()),
        _ => {}
    }
}
pub(super) fn pattern(p: &CPattern, module: &[String]) -> String {
    match p {
        CPattern::Var(n) => n.clone(),
        CPattern::Wildcard(_) => "_".into(),
        CPattern::Literal(PatternLiteral::Int(n)) => n.to_string(),
        CPattern::Literal(PatternLiteral::Str(s)) => quoted(s),
        CPattern::Ctor(r, ps) => format!(
            "({}{})",
            global_source(&reference(r, module)),
            ps.iter()
                .map(|p| format!(" {}", pattern(&p.node, module)))
                .collect::<String>()
        ),
        CPattern::Tuple(ps) => format!(
            "({})",
            ps.iter()
                .map(|p| pattern(&p.node, module))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        CPattern::Cons(a, b) => format!(
            "({} : {})",
            pattern(&a.node, module),
            pattern(&b.node, module)
        ),
        CPattern::Nil => "[]".into(),
        CPattern::Unit => "()".into(),
        CPattern::Record(ns) => format!("{{ {} }}", ns.join(", ")),
        CPattern::As(p, n) => format!("({} as {n})", pattern(&p.node, module)),
    }
}
