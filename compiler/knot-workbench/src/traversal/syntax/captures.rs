//! Render a lexical capture graph as ordinary local bindings instead of
//! recursively substituting it at every reference. Scope/name pairs identify
//! activations; equal values or equal source spans never merge captures.
use super::*;
pub(super) struct Captures {
    prefix: String,
    names: BTreeMap<(usize, String), String>,
    pending: Vec<(String, Site)>,
}
impl Captures {
    pub(super) fn reference(
        &mut self,
        scope: usize,
        name: &str,
        site: &Site,
    ) -> Result<String, EditError> {
        let key = (scope, name.to_string());
        if let Some(name) = self.names.get(&key) {
            return Ok(name.clone());
        }
        if self.pending.len() >= 10_000 {
            return unsupported("source rendering exceeded 10000 lexical captures");
        }
        let name = format!("{}{}", self.prefix, self.pending.len());
        self.names.insert(key, name.clone());
        self.pending.push((name.clone(), site.clone()));
        Ok(name)
    }
}
pub(super) fn render_captured(
    site: &Site,
    scopes: &Scopes,
    patches: Replacements<'_>,
    typing: Option<&Typing>,
) -> Result<String, EditError> {
    let mut names = BTreeSet::new();
    // Include binder names as well as references: a generated capture binding
    // must not become shadowed by a lambda/let in a rendered capture's body.
    let mut pending = vec![&site.expr];
    if let Some(recipe) = &site.recipe {
        pending.push(&recipe.root.expr);
    }
    pending.extend(
        scopes
            .iter()
            .flat_map(|scope| scope.values())
            .filter_map(|s| s.recipe.as_ref().map(|r| &r.root.expr)),
    );
    if let Some((replacements, _)) = patches {
        pending.extend(replacements.values());
    }
    pending.extend(
        scopes
            .iter()
            .flat_map(|scope| scope.values().map(|s| &s.expr)),
    );
    while let Some(expr) = pending.pop() {
        match &expr.node {
            CExpr::Var(r) | CExpr::Ctor(r) => {
                names.insert(reference(r, &[]).name);
            }
            CExpr::App(a, b) => {
                pending.push(a);
                pending.push(b);
            }
            CExpr::BinOp(r, a, b) => {
                names.insert(reference(r, &[]).name);
                pending.push(a);
                pending.push(b);
            }
            CExpr::Negate(e) | CExpr::FieldAccess(e, _) | CExpr::Annotated(_, e) => pending.push(e),
            CExpr::Tuple(es) | CExpr::List(es) => pending.extend(es),
            CExpr::Record(fs) => pending.extend(fs.iter().map(|(_, e)| e)),
            CExpr::RecordUpdate(base, fs) => {
                pending.push(base);
                pending.extend(fs.iter().map(|(_, e)| e));
            }
            CExpr::If(c, t, f) => {
                pending.push(c);
                pending.push(t);
                pending.push(f);
            }
            CExpr::Lambda(ps, body) => {
                for p in ps {
                    bind_names(&p.node, &mut names);
                }
                pending.push(body);
            }
            CExpr::Let(bs, body) => {
                for (p, e) in bs {
                    bind_names(&p.node, &mut names);
                    pending.push(e);
                }
                pending.push(body);
            }
            CExpr::Case(value, arms) => {
                pending.push(value);
                for (p, e) in arms {
                    bind_names(&p.node, &mut names);
                    pending.push(e);
                }
            }
            _ => {}
        }
    }
    if let Some(typing) = typing {
        names.insert(typing.prefix.clone());
    }
    let mut prefix = "twineCapture".to_string();
    while names.iter().any(|n| n.starts_with(&prefix)) {
        prefix.push('x');
    }
    let captures = RefCell::new(Captures {
        prefix,
        names: BTreeMap::new(),
        pending: vec![],
    });
    let root = render_inner(
        site,
        scopes,
        &BTreeSet::new(),
        0,
        patches,
        typing,
        &captures,
    )?;
    let mut bindings = String::new();
    let mut cursor = 0;
    loop {
        let item = captures.borrow().pending.get(cursor).cloned();
        let Some((name, site)) = item else {
            break;
        };
        cursor += 1;
        let body = render_inner(
            &site,
            scopes,
            &BTreeSet::new(),
            0,
            patches,
            typing,
            &captures,
        )?;
        if root
            .len()
            .saturating_add(bindings.len())
            .saturating_add(body.len().saturating_mul(3))
            .saturating_add(name.len() + 32)
            > 1024 * 1024
        {
            return Err(EditError::SourceTooLarge);
        }
        bindings.push_str(&format!("{name} =\n{}", indent(&body)));
    }
    if cursor == 0 {
        return Ok(root);
    }
    let source = format!("(let\n{}in\n{})", indent(&bindings), indent(&root));
    if source.len() > 1024 * 1024 {
        return Err(EditError::SourceTooLarge);
    }
    Ok(source)
}
